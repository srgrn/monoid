use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use symphonia::core::audio::SampleBuffer;
use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::Emitter;

const SUPPORTED_EXTENSIONS: &[&str] = &[
    "aac", "aiff", "caf", "flac", "m4a", "mkv", "mp3", "mp4", "ogg", "wav",
];

#[derive(Default)]
struct ConversionState {
    cancel_requested: bool,
    running: bool,
}

struct AppState(Arc<Mutex<ConversionState>>);

struct ProgressReader<R: Read + Seek + Send + Sync> {
    inner: R,
    bytes_read: Arc<AtomicU64>,
    total_size: u64,
}

impl<R: Read + Seek + Send + Sync> Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let result = self.inner.read(buf);
        if let Ok(bytes) = result {
            self.bytes_read.fetch_add(bytes as u64, Ordering::Relaxed);
        }
        result
    }
}

impl<R: Read + Seek + Send + Sync> Seek for ProgressReader<R> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl<R> symphonia::core::io::MediaSource for ProgressReader<R>
where
    R: Read + Seek + Send + Sync,
{
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.total_size)
    }
}

#[derive(serde::Serialize)]
struct AudioInfo {
    channels: u32,
    sample_rate: u32,
    bits_per_sample: u32,
    duration_seconds: Option<f64>,
}

#[derive(serde::Serialize)]
struct GetAudioInfoResponse {
    success: bool,
    data: Option<AudioInfo>,
    error: Option<String>,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchConversionOptions {
    skip_existing_outputs: bool,
    stop_on_error: bool,
    output_dir: Option<String>,
    filename_template: Option<String>,
    overwrite_policy: Option<OverwritePolicy>,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum OverwritePolicy {
    #[default]
    Forbid,
    Overwrite,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchItemEvent {
    file_path: String,
    status: String,
    output_path: Option<String>,
    message: Option<String>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchProgressEvent {
    total: usize,
    completed: usize,
    succeeded: usize,
    failed: usize,
    skipped: usize,
    cancelled: usize,
    current_file: Option<String>,
    current_file_progress: f64,
    overall_progress: f64,
    running: bool,
    message: String,
}

#[derive(Default)]
struct BatchCounters {
    completed: usize,
    succeeded: usize,
    failed: usize,
    skipped: usize,
    cancelled: usize,
}

fn is_supported_audio_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|supported| supported.eq_ignore_ascii_case(extension))
        })
        .unwrap_or(false)
}

fn render_output_filename(template: &str, input_path: &Path) -> Result<String, String> {
    let stem = input_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or("Input file name is missing")?;
    let original_ext = input_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");

    let rendered = template
        .trim()
        .replace("{stem}", stem)
        .replace("{original_ext}", original_ext)
        .replace("{ext}", "wav");
    if rendered.is_empty() {
        return Err("Filename template resolved to an empty value".to_string());
    }

    let filename = if rendered.to_ascii_lowercase().ends_with(".wav") {
        rendered
    } else {
        format!("{rendered}.wav")
    };

    if Path::new(&filename).components().count() != 1 {
        return Err("Filename template must not include path separators".to_string());
    }

    Ok(filename)
}

fn build_output_path(path: &Path, options: &BatchConversionOptions) -> Result<PathBuf, String> {
    let filename_template = options
        .filename_template
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("{stem}_mono");
    let filename = render_output_filename(filename_template, path)?;

    let output_dir = options
        .output_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));

    Ok(output_dir.join(filename))
}

fn ensure_output_path_allowed(output_path: &Path, policy: OverwritePolicy) -> Result<(), String> {
    if output_path.exists() && policy != OverwritePolicy::Overwrite {
        return Err(format!(
            "Output file already exists: {}",
            output_path.display()
        ));
    }

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to prepare output directory: {error}"))?;
    }

    Ok(())
}

fn normalized_f32_to_i16(sample: f32) -> i16 {
    let clamped = if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    };

    let scale = if clamped < 0.0 {
        f32::from(i16::MAX) + 1.0
    } else {
        f32::from(i16::MAX)
    };
    let scaled = (clamped * scale).round();
    scaled.clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

fn collect_audio_files(folder_path: &Path) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    collect_audio_files_recursive(folder_path, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_audio_files_recursive(path: &Path, files: &mut Vec<String>) -> Result<(), String> {
    for entry in fs::read_dir(path).map_err(|err| format!("Failed to read directory: {err}"))? {
        let entry = entry.map_err(|err| format!("Failed to read directory entry: {err}"))?;
        let entry_path = entry.path();

        if entry_path.is_dir() {
            collect_audio_files_recursive(&entry_path, files)?;
        } else if is_supported_audio_path(&entry_path) {
            files.push(entry_path.to_string_lossy().to_string());
        }
    }

    Ok(())
}

fn current_cancel_requested(state: &Arc<Mutex<ConversionState>>) -> bool {
    state.lock().unwrap().cancel_requested
}

fn set_running_state(state: &Arc<Mutex<ConversionState>>, running: bool) {
    let mut guard = state.lock().unwrap();
    guard.running = running;
    if !running {
        guard.cancel_requested = false;
    }
}

fn emit_batch_item(
    app: &tauri::AppHandle,
    file_path: &str,
    status: &str,
    output_path: Option<&Path>,
    message: Option<String>,
) {
    let payload = BatchItemEvent {
        file_path: file_path.to_string(),
        status: status.to_string(),
        output_path: output_path.map(|path| path.to_string_lossy().to_string()),
        message,
    };

    let _ = app.emit("batch-item", payload);
}

fn build_progress_event(
    total: usize,
    counters: &BatchCounters,
    current_file: Option<&str>,
    current_file_progress: f64,
    running: bool,
    message: impl Into<String>,
) -> BatchProgressEvent {
    let base_completed = counters.completed as f64;
    let file_fraction = if total == 0 {
        0.0
    } else {
        (base_completed + current_file_progress.clamp(0.0, 100.0) / 100.0) / total as f64
    };

    BatchProgressEvent {
        total,
        completed: counters.completed,
        succeeded: counters.succeeded,
        failed: counters.failed,
        skipped: counters.skipped,
        cancelled: counters.cancelled,
        current_file: current_file.map(|file| file.to_string()),
        current_file_progress,
        overall_progress: (file_fraction * 100.0).clamp(0.0, 100.0),
        running,
        message: message.into(),
    }
}

fn emit_batch_progress(
    app: &tauri::AppHandle,
    total: usize,
    counters: &BatchCounters,
    current_file: Option<&str>,
    current_file_progress: f64,
    running: bool,
    message: impl Into<String>,
) {
    let payload = build_progress_event(
        total,
        counters,
        current_file,
        current_file_progress,
        running,
        message,
    );
    let _ = app.emit("batch-progress", payload);
}

fn emit_batch_finished(
    app: &tauri::AppHandle,
    total: usize,
    counters: &BatchCounters,
    message: impl Into<String>,
) {
    let payload = build_progress_event(total, counters, None, 100.0, false, message);
    let _ = app.emit("batch-finished", payload);
}

fn get_audio_info_inner(file_path: &str) -> Result<AudioInfo, String> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = File::open(file_path).map_err(|err| format!("Failed to open file: {err}"))?;
    let media_source_stream = MediaSourceStream::new(Box::new(file), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            media_source_stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|err| format!("Unsupported format: {err}"))?;

    let format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| "No supported audio tracks".to_string())?;

    let channels = track
        .codec_params
        .channels
        .map(|value| value.count() as u32)
        .unwrap_or(0);
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| "Unknown sample rate".to_string())?;
    let bits_per_sample = track.codec_params.bits_per_sample.unwrap_or(16);
    let duration_seconds = track
        .codec_params
        .n_frames
        .map(|frames| frames as f64 / sample_rate as f64);

    Ok(AudioInfo {
        channels,
        sample_rate,
        bits_per_sample,
        duration_seconds,
    })
}

fn convert_file_to_mono(
    state: &Arc<Mutex<ConversionState>>,
    file_path: &Path,
    output_path: &Path,
    mut on_progress: impl FnMut(f64),
) -> Result<(), String> {
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let input = File::open(file_path).map_err(|err| format!("Failed to open file: {err}"))?;
    let total_size = input
        .metadata()
        .map_err(|err| format!("Failed to get file metadata: {err}"))?
        .len();
    let bytes_read = Arc::new(AtomicU64::new(0));
    let progress_reader = ProgressReader {
        inner: input,
        bytes_read: bytes_read.clone(),
        total_size,
    };
    let media_source_stream = MediaSourceStream::new(Box::new(progress_reader), Default::default());

    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            media_source_stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|err| format!("Unsupported format: {err}"))?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| "No supported audio tracks".to_string())?;
    let track_id = track.id;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| "Unknown sample rate".to_string())?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|err| format!("Unsupported codec: {err}"))?;

    let wav_file = File::create(output_path)
        .map_err(|err| format!("Failed to create WAV file: {err}"))?;
    let mut writer = hound::WavWriter::new(
        BufWriter::new(wav_file),
        hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .map_err(|err| format!("Failed to create WAV writer: {err}"))?;

    let mut last_reported_progress = -1.0f64;
    on_progress(0.0);

    // Reuse sample buffer across packets to avoid per-packet allocation
    let mut sample_buffer: Option<SampleBuffer<f32>> = None;
    // Batch mono samples before writing
    let mut mono_batch: Vec<i16> = Vec::with_capacity(8192);

    loop {
        if current_cancel_requested(state) {
            let _ = fs::remove_file(output_path);
            return Err("Conversion cancelled".to_string());
        }

        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(symphonia::core::errors::Error::ResetRequired) => continue,
            Err(_) => break,
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = decoder
            .decode(&packet)
            .map_err(|err| format!("Decode error: {err}"))?;
        let spec = *decoded.spec();
        let channels = spec.channels.count();

        // Reuse or create sample buffer with sufficient capacity
        let buf = sample_buffer.get_or_insert_with(|| {
            SampleBuffer::<f32>::new(decoded.capacity() as u64, spec)
        });
        if buf.capacity() < decoded.capacity() {
            *buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        }
        buf.copy_interleaved_ref(decoded);

        // Convert to mono in batch
        mono_batch.clear();
        let inv_channels = 1.0 / channels as f32;
        for frame in buf.samples().chunks(channels) {
            let sum: f32 = frame.iter().copied().sum();
            let mono = (sum * inv_channels).clamp(-1.0, 1.0);
            mono_batch.push(normalized_f32_to_i16(mono));
        }

        // Write batch
        for &sample in &mono_batch {
            writer
                .write_sample(sample)
                .map_err(|_| "Write error".to_string())?;
        }

        let progress = if total_size == 0 {
            100.0
        } else {
            (bytes_read.load(Ordering::Relaxed) as f64 / total_size as f64 * 100.0).clamp(0.0, 100.0)
        };
        if (progress - last_reported_progress).abs() >= 1.0 {
            on_progress(progress);
            last_reported_progress = progress;
        }
    }

    writer
        .finalize()
        .map_err(|_| "Finalize error".to_string())?;
    on_progress(100.0);
    Ok(())
}

fn unique_paths(file_paths: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::with_capacity(file_paths.len());
    let mut unique = Vec::with_capacity(file_paths.len());
    for file_path in file_paths {
        if file_path.trim().is_empty() || !seen.insert(file_path.clone()) {
            continue;
        }
        unique.push(file_path);
    }
    unique
}

fn start_batch_job(
    app: tauri::AppHandle,
    shared_state: Arc<Mutex<ConversionState>>,
    file_paths: Vec<String>,
    options: BatchConversionOptions,
) {
    tauri::async_runtime::spawn(async move {
        let queue = unique_paths(file_paths);
        let total = queue.len();
        let mut counters = BatchCounters::default();
        let mut stopped_after_error = false;

        emit_batch_progress(
            &app,
            total,
            &counters,
            None,
            0.0,
            true,
            "Batch conversion started",
        );

        for file_path in &queue {
            if current_cancel_requested(&shared_state) {
                counters.cancelled += 1;
                counters.completed += 1;
                emit_batch_item(
                    &app,
                    file_path,
                    "cancelled",
                    None,
                    Some("Cancelled before processing".to_string()),
                );
                continue;
            }

            let source_path = PathBuf::from(file_path);
            if !is_supported_audio_path(&source_path) {
                counters.failed += 1;
                counters.completed += 1;
                emit_batch_item(
                    &app,
                    file_path,
                    "failed",
                    None,
                    Some("Unsupported file type".to_string()),
                );
                emit_batch_progress(
                    &app,
                    total,
                    &counters,
                    None,
                    0.0,
                    true,
                    format!("Failed: {}", source_path.display()),
                );
                if options.stop_on_error {
                    stopped_after_error = true;
                    break;
                }
                continue;
            }

            let output_path = match build_output_path(&source_path, &options) {
                Ok(path) => path,
                Err(error) => {
                    counters.failed += 1;
                    counters.completed += 1;
                    emit_batch_item(&app, file_path, "failed", None, Some(error.clone()));
                    emit_batch_progress(
                        &app,
                        total,
                        &counters,
                        None,
                        0.0,
                        true,
                        format!("Failed: {}", source_path.display()),
                    );
                    if options.stop_on_error {
                        stopped_after_error = true;
                        break;
                    }
                    continue;
                }
            };
            if options.skip_existing_outputs && output_path.exists() {
                counters.skipped += 1;
                counters.completed += 1;
                emit_batch_item(
                    &app,
                    file_path,
                    "skipped",
                    Some(&output_path),
                    Some("Output already exists".to_string()),
                );
                emit_batch_progress(
                    &app,
                    total,
                    &counters,
                    None,
                    0.0,
                    true,
                    format!("Skipped existing output for {}", source_path.display()),
                );
                continue;
            }

            if let Err(error) = ensure_output_path_allowed(
                &output_path,
                options.overwrite_policy.unwrap_or_default(),
            ) {
                counters.failed += 1;
                counters.completed += 1;
                emit_batch_item(
                    &app,
                    file_path,
                    "failed",
                    Some(&output_path),
                    Some(error.clone()),
                );
                emit_batch_progress(
                    &app,
                    total,
                    &counters,
                    None,
                    0.0,
                    true,
                    format!("Failed: {}", source_path.display()),
                );
                if options.stop_on_error {
                    stopped_after_error = true;
                    break;
                }
                continue;
            }

            emit_batch_item(
                &app,
                file_path,
                "processing",
                Some(&output_path),
                Some("Converting to mono".to_string()),
            );

            let source_label = source_path.to_string_lossy().to_string();
            let progress_app = app.clone();
            let progress_counters = counters.completed;
            let progress_total = total;
            let progress_message = format!("Processing {}", source_path.display());
            let progress_state_snapshot = BatchCounters {
                completed: progress_counters,
                succeeded: counters.succeeded,
                failed: counters.failed,
                skipped: counters.skipped,
                cancelled: counters.cancelled,
            };

            let result =
                convert_file_to_mono(&shared_state, &source_path, &output_path, move |progress| {
                    emit_batch_progress(
                        &progress_app,
                        progress_total,
                        &progress_state_snapshot,
                        Some(&source_label),
                        progress,
                        true,
                        &progress_message,
                    );
                });

            match result {
                Ok(()) => {
                    counters.succeeded += 1;
                    counters.completed += 1;
                    emit_batch_item(
                        &app,
                        file_path,
                        "done",
                        Some(&output_path),
                        Some("Converted".to_string()),
                    );
                    emit_batch_progress(
                        &app,
                        total,
                        &counters,
                        None,
                        0.0,
                        true,
                        format!("Finished {}", source_path.display()),
                    );
                }
                Err(error) if error == "Conversion cancelled" => {
                    counters.cancelled += 1;
                    counters.completed += 1;
                    emit_batch_item(
                        &app,
                        file_path,
                        "cancelled",
                        Some(&output_path),
                        Some("Cancelled during conversion".to_string()),
                    );
                }
                Err(error) => {
                    let _ = fs::remove_file(&output_path);
                    counters.failed += 1;
                    counters.completed += 1;
                    emit_batch_item(
                        &app,
                        file_path,
                        "failed",
                        Some(&output_path),
                        Some(error.clone()),
                    );
                    emit_batch_progress(
                        &app,
                        total,
                        &counters,
                        None,
                        0.0,
                        true,
                        format!("Failed {}", source_path.display()),
                    );
                    if options.stop_on_error {
                        stopped_after_error = true;
                        break;
                    }
                }
            }
        }

        if current_cancel_requested(&shared_state) {
            for file_path in queue.iter().skip(counters.completed) {
                counters.cancelled += 1;
                counters.completed += 1;
                emit_batch_item(
                    &app,
                    file_path,
                    "cancelled",
                    None,
                    Some("Cancelled before processing".to_string()),
                );
            }
        } else if stopped_after_error {
            for file_path in queue.iter().skip(counters.completed) {
                counters.cancelled += 1;
                counters.completed += 1;
                emit_batch_item(
                    &app,
                    file_path,
                    "cancelled",
                    None,
                    Some("Stopped after a previous failure".to_string()),
                );
            }
        }

        let final_message = if current_cancel_requested(&shared_state) {
            format!(
                "Batch cancelled. {} converted, {} skipped, {} failed.",
                counters.succeeded, counters.skipped, counters.failed
            )
        } else if counters.failed > 0 {
            format!(
                "Batch finished with failures. {} converted, {} skipped, {} failed.",
                counters.succeeded, counters.skipped, counters.failed
            )
        } else {
            format!(
                "Batch complete. {} converted, {} skipped.",
                counters.succeeded, counters.skipped
            )
        };

        set_running_state(&shared_state, false);
        emit_batch_finished(&app, total, &counters, final_message);
    });
}

#[tauri::command]
fn cancel_conversion(state: tauri::State<AppState>) {
    let mut guard = state.0.lock().unwrap();
    if guard.running {
        guard.cancel_requested = true;
    }
}

#[tauri::command]
fn get_audio_info(file_path: String) -> GetAudioInfoResponse {
    match get_audio_info_inner(&file_path) {
        Ok(info) => GetAudioInfoResponse {
            success: true,
            data: Some(info),
            error: None,
        },
        Err(error) => GetAudioInfoResponse {
            success: false,
            data: None,
            error: Some(error),
        },
    }
}

#[tauri::command]
fn list_supported_audio_files(folder_path: String) -> Result<Vec<String>, String> {
    collect_audio_files(Path::new(&folder_path))
}

#[tauri::command]
fn start_batch_conversion(
    app: tauri::AppHandle,
    state: tauri::State<AppState>,
    file_paths: Vec<String>,
    options: BatchConversionOptions,
) -> Result<(), String> {
    if file_paths.is_empty() {
        return Err("Add at least one file to start a batch.".to_string());
    }

    {
        let mut guard = state.0.lock().unwrap();
        if guard.running {
            return Err("A conversion is already running.".to_string());
        }
        guard.running = true;
        guard.cancel_requested = false;
    }

    start_batch_job(app, state.0.clone(), file_paths, options);
    Ok(())
}

#[tauri::command]
fn convert_to_mono(
    app: tauri::AppHandle,
    state: tauri::State<AppState>,
    file_path: String,
) -> Result<(), String> {
    start_batch_conversion(
        app,
        state,
        vec![file_path],
        BatchConversionOptions {
            skip_existing_outputs: false,
            stop_on_error: true,
            output_dir: None,
            filename_template: None,
            overwrite_policy: None,
        },
    )
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState(Arc::new(Mutex::new(ConversionState::default()))))
        .invoke_handler(tauri::generate_handler![
            cancel_conversion,
            convert_to_mono,
            get_audio_info,
            list_supported_audio_files,
            start_batch_conversion
        ])
        .setup(|app| {
            let check_updates =
                MenuItemBuilder::with_id("check_updates", "Check for Updates...").build(app)?;

            let app_menu = SubmenuBuilder::new(app, "Monoid")
                .about(None)
                .separator()
                .item(&check_updates)
                .separator()
                .services()
                .separator()
                .hide()
                .hide_others()
                .show_all()
                .separator()
                .quit()
                .build()?;

            let edit_menu = SubmenuBuilder::new(app, "Edit")
                .undo()
                .redo()
                .separator()
                .cut()
                .copy()
                .paste()
                .select_all()
                .build()?;

            let window_menu = SubmenuBuilder::new(app, "Window")
                .minimize()
                .separator()
                .close_window()
                .build()?;

            let menu = MenuBuilder::new(app)
                .item(&app_menu)
                .item(&edit_menu)
                .item(&window_menu)
                .build()?;

            app.set_menu(menu)?;

            app.on_menu_event(move |app_handle, event| {
                if event.id() == "check_updates" {
                    let _ = app_handle.emit("check-for-updates", ());
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{
        build_output_path, collect_audio_files, ensure_output_path_allowed,
        is_supported_audio_path, normalized_f32_to_i16, render_output_filename, unique_paths,
        BatchConversionOptions, OverwritePolicy,
    };
    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn build_output_path_replaces_extension() {
        let output = build_output_path(
            Path::new("/tmp/example.track.mp3"),
            &BatchConversionOptions {
                skip_existing_outputs: false,
                stop_on_error: true,
                output_dir: None,
                filename_template: None,
                overwrite_policy: None,
            },
        )
        .unwrap();
        assert_eq!(output.to_string_lossy(), "/tmp/example.track_mono.wav");
    }

    #[test]
    fn build_output_path_supports_custom_directory_and_template() {
        let output = build_output_path(
            Path::new("/tmp/example.track.mp3"),
            &BatchConversionOptions {
                skip_existing_outputs: false,
                stop_on_error: true,
                output_dir: Some("/var/tmp/exports".to_string()),
                filename_template: Some("{stem}-batch.{ext}".to_string()),
                overwrite_policy: Some(OverwritePolicy::Overwrite),
            },
        )
        .unwrap();
        assert_eq!(output.to_string_lossy(), "/var/tmp/exports/example.track-batch.wav");
    }

    #[test]
    fn render_output_filename_rejects_path_segments() {
        let error = render_output_filename("../{stem}", Path::new("/tmp/example.mp3")).unwrap_err();
        assert!(error.contains("path separators"));
    }

    #[test]
    fn ensure_output_path_allowed_rejects_existing_files_without_overwrite() {
        let base = std::env::temp_dir().join(format!(
            "monoid-output-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        let output = base.join("existing.wav");
        fs::write(&output, b"test").unwrap();

        let error = ensure_output_path_allowed(&output, OverwritePolicy::Forbid).unwrap_err();
        assert!(error.contains("already exists"));

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn normalization_clamps_non_finite_and_out_of_range_values() {
        assert_eq!(normalized_f32_to_i16(f32::NAN), 0);
        assert_eq!(normalized_f32_to_i16(f32::INFINITY), 0);
        assert_eq!(normalized_f32_to_i16(1.5), i16::MAX);
        assert_eq!(normalized_f32_to_i16(-1.5), i16::MIN);
    }

    #[test]
    fn support_check_handles_extension_case() {
        assert!(is_supported_audio_path(Path::new("/tmp/song.FLAC")));
        assert!(!is_supported_audio_path(Path::new("/tmp/notes.txt")));
    }

    #[test]
    fn unique_paths_preserves_order() {
        assert_eq!(
            unique_paths(vec![
                "".to_string(),
                "/tmp/a.wav".to_string(),
                "/tmp/a.wav".to_string(),
                "/tmp/b.wav".to_string(),
            ]),
            vec!["/tmp/a.wav".to_string(), "/tmp/b.wav".to_string()]
        );
    }

    #[test]
    fn collect_audio_files_walks_nested_directories() {
        let base = std::env::temp_dir().join(format!(
            "monoid-batch-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let nested = base.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(base.join("one.wav"), b"test").unwrap();
        fs::write(nested.join("two.mp3"), b"test").unwrap();
        fs::write(nested.join("three.txt"), b"test").unwrap();

        let files = collect_audio_files(&base).unwrap();

        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|file| file.ends_with("one.wav")));
        assert!(files.iter().any(|file| file.ends_with("two.mp3")));

        fs::remove_dir_all(base).unwrap();
    }

}

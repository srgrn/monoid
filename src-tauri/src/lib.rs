// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
use serde::Deserialize;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use symphonia::core::audio::{AudioBuffer, AudioBufferRef, Signal};
use symphonia::core::conv::IntoSample;
use symphonia::core::sample::Sample;
use tauri::Emitter;

struct CancelFlag(Arc<Mutex<bool>>);

struct ProgressReader<R: Read + Seek + Send + Sync> {
    inner: R,
    bytes_read: Arc<Mutex<u64>>,
    total_size: u64,
}

impl<R: Read + Seek + Send + Sync> Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let res = self.inner.read(buf);
        if let Ok(n) = res {
            *self.bytes_read.lock().unwrap() += n as u64;
        }
        res
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

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConvertRequest {
    file_path: String,
    output_dir: Option<String>,
    filename_template: Option<String>,
    overwrite_policy: Option<OverwritePolicy>,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum OverwritePolicy {
    Forbid,
    Overwrite,
}

impl Default for OverwritePolicy {
    fn default() -> Self {
        Self::Forbid
    }
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

    let output_path = Path::new(&filename);
    if output_path.components().count() != 1 {
        return Err("Filename template must not include path separators".to_string());
    }

    Ok(filename)
}

fn resolve_output_path(request: &ConvertRequest) -> Result<PathBuf, String> {
    let input_path = Path::new(&request.file_path);
    if request.file_path.trim().is_empty() {
        return Err("Input file path is required".to_string());
    }

    let filename_template = request
        .filename_template
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("{stem}_mono");
    let filename = render_output_filename(filename_template, input_path)?;

    let output_dir = request
        .output_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| input_path.parent().map(Path::to_path_buf))
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
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to prepare output directory: {error}"))?;
    }

    Ok(())
}

fn build_output_path(file_path: &str) -> Result<PathBuf, String> {
    let input_path = Path::new(file_path);
    let stem = input_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| "Unable to determine a valid output filename".to_string())?;

    Ok(input_path.with_file_name(format!("{stem}_mono.wav")))
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

fn mono_frame_to_i16<T>(channels: &[T]) -> Option<i16>
where
    T: Sample + IntoSample<f32> + Copy,
{
    if channels.is_empty() {
        return None;
    }

    let sum = channels
        .iter()
        .copied()
        .map(IntoSample::<f32>::into_sample)
        .sum::<f32>();
    let average = sum / channels.len() as f32;
    Some(normalized_f32_to_i16(average))
}

fn write_mono_buffer<T>(
    buf: &AudioBuffer<T>,
    writer: &mut hound::WavWriter<std::io::BufWriter<std::fs::File>>,
) -> Result<(), hound::Error>
where
    T: Sample + IntoSample<f32> + Copy,
{
    let channels = buf.spec().channels.count();
    if channels == 0 {
        return Ok(());
    }

    let mut frame = Vec::with_capacity(channels);
    for i in 0..buf.frames() {
        frame.clear();
        for ch in 0..channels {
            frame.push(buf.chan(ch)[i]);
        }

        if let Some(mono) = mono_frame_to_i16(&frame) {
            writer.write_sample(mono)?;
        }
    }

    Ok(())
}

fn write_audio_buffer(
    decoded: AudioBufferRef<'_>,
    writer: &mut hound::WavWriter<std::io::BufWriter<std::fs::File>>,
) -> Result<(), hound::Error> {
    match decoded {
        AudioBufferRef::U8(buf) => write_mono_buffer(buf.as_ref(), writer),
        AudioBufferRef::U16(buf) => write_mono_buffer(buf.as_ref(), writer),
        AudioBufferRef::U24(buf) => write_mono_buffer(buf.as_ref(), writer),
        AudioBufferRef::U32(buf) => write_mono_buffer(buf.as_ref(), writer),
        AudioBufferRef::S8(buf) => write_mono_buffer(buf.as_ref(), writer),
        AudioBufferRef::S16(buf) => write_mono_buffer(buf.as_ref(), writer),
        AudioBufferRef::S24(buf) => write_mono_buffer(buf.as_ref(), writer),
        AudioBufferRef::S32(buf) => write_mono_buffer(buf.as_ref(), writer),
        AudioBufferRef::F32(buf) => write_mono_buffer(buf.as_ref(), writer),
        AudioBufferRef::F64(buf) => write_mono_buffer(buf.as_ref(), writer),
    }
}

#[tauri::command]
fn cancel_conversion(state: tauri::State<CancelFlag>) {
    *state.0.lock().unwrap() = true;
}

#[tauri::command]
fn get_audio_info(file_path: String) -> GetAudioInfoResponse {
    use std::fs::File;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    match (|| -> Result<AudioInfo, String> {
        let file = File::open(&file_path).map_err(|e| format!("Failed to open file: {}", e))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let hint = Hint::new();
        let format_opts = FormatOptions::default();
        let metadata_opts = MetadataOptions::default();
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .map_err(|e| format!("Unsupported format: {}", e))?;

        let format = probed.format;

        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
            .ok_or("No supported audio tracks")?;

        let channels = track
            .codec_params
            .channels
            .map(|c| c.count() as u32)
            .unwrap_or(0);
        let sample_rate = track
            .codec_params
            .sample_rate
            .ok_or("Unknown sample rate")?;
        let bits_per_sample = track.codec_params.bits_per_sample.unwrap_or(16);

        let duration_seconds = track
            .codec_params
            .n_frames
            .map(|n| n as f64 / sample_rate as f64);

        Ok(AudioInfo {
            channels,
            sample_rate,
            bits_per_sample,
            duration_seconds,
        })
    })() {
        Ok(info) => GetAudioInfoResponse {
            success: true,
            data: Some(info),
            error: None,
        },
        Err(e) => GetAudioInfoResponse {
            success: false,
            data: None,
            error: Some(e),
        },
    }
}

#[tauri::command]
fn convert_to_mono(
    app: tauri::AppHandle,
    state: tauri::State<CancelFlag>,
    request: ConvertRequest,
) -> Result<(), String> {
    {
        let mut cancel = state.0.lock().unwrap();
        *cancel = false;
    }
    let cancel_flag = state.0.clone();
    let app_clone = app.clone();
    let output_path = resolve_output_path(&request)?;
    ensure_output_path_allowed(&output_path, request.overwrite_policy.unwrap_or_default())?;
    tauri::async_runtime::spawn(async move {
        let file_path = request.file_path;
        println!("Converting file: {}", file_path);
        use std::fs::File;
        use symphonia::core::codecs::DecoderOptions;
        use symphonia::core::formats::FormatOptions;
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        use symphonia::core::probe::Hint;

        // Open the input file
        let file = match File::open(&file_path) {
            Ok(f) => f,
            Err(e) => {
                println!("Failed to open file: {:?}", e);
                let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": format!("Failed to open file: {}", e) }));
                return;
            }
        };
        let total_size = match file.metadata() {
            Ok(m) => m.len(),
            Err(e) => {
                println!("Failed to get file metadata: {:?}", e);
                let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": format!("Failed to get file metadata: {}", e) }));
                return;
            }
        };
        let bytes_read = Arc::new(Mutex::new(0u64));
        let progress_reader = ProgressReader {
            inner: file,
            bytes_read: bytes_read.clone(),
            total_size,
        };
        let mss = MediaSourceStream::new(Box::new(progress_reader), Default::default());

        // Probe the format
        let hint = Hint::new();
        let format_opts = FormatOptions::default();
        let metadata_opts = MetadataOptions::default();
        let probed = match symphonia::default::get_probe().format(
            &hint,
            mss,
            &format_opts,
            &metadata_opts,
        ) {
            Ok(p) => p,
            Err(e) => {
                println!("Unsupported format error: {:?}", e);
                let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": format!("Unsupported format: {}", e) }));
                return;
            }
        };

        let mut format = probed.format;

        // Get the default track
        let track = match format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        {
            Some(t) => t,
            None => {
                let _ = app_clone.emit(
                    "conversion-result",
                    serde_json::json!({ "success": false, "error": "No supported audio tracks" }),
                );
                return;
            }
        };

        let track_id = track.id;
        let sample_rate = match track.codec_params.sample_rate {
            Some(sr) => sr,
            None => {
                let _ = app_clone.emit(
                    "conversion-result",
                    serde_json::json!({ "success": false, "error": "Unknown sample rate" }),
                );
                return;
            }
        };

        // Create a decoder
        let dec_opts = DecoderOptions::default();
        let mut decoder = match symphonia::default::get_codecs()
            .make(&track.codec_params, &dec_opts)
        {
            Ok(d) => d,
            Err(e) => {
                let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": format!("Unsupported codec: {}", e) }));
                return;
            }
        };

        // Prepare output WAV file with hound
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = match hound::WavWriter::create(&output_path, spec) {
            Ok(w) => w,
            Err(e) => {
                let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": format!("Failed to create WAV file: {}", e) }));
                return;
            }
        };

        // Emit start
        let _ = app_clone.emit("progress", "Starting conversion...");

        // Decode and convert
        let mut packet_count = 0;
        loop {
            if *cancel_flag.lock().unwrap() {
                let _ = std::fs::remove_file(&output_path);
                let _ = app_clone.emit(
                    "conversion-result",
                    serde_json::json!({ "success": false, "error": "Conversion cancelled" }),
                );
                return;
            }

            let packet = match format.next_packet() {
                Ok(packet) => packet,
                Err(symphonia::core::errors::Error::ResetRequired) => continue,
                Err(_) => break,
            };

            if packet.track_id() != track_id {
                continue;
            }

            packet_count += 1;
            if packet_count % 100 == 0 {
                let current_bytes = *bytes_read.lock().unwrap();
                let progress = (current_bytes as f64 / total_size as f64 * 100.0) as f64;
                println!("Progress: {:.1}% ({} packets)", progress, packet_count);
                let _ = app_clone.emit("progress", format!("{:.1}%", progress));
            }

            let decoded = match decoder.decode(&packet) {
                Ok(d) => d,
                Err(e) => {
                    let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": format!("Decode error: {}", e) }));
                    return;
                }
            };

            if write_audio_buffer(decoded, &mut writer).is_err() {
                let _ = app_clone.emit(
                    "conversion-result",
                    serde_json::json!({ "success": false, "error": "Write error" }),
                );
                return;
            }
        }

        if writer.finalize().is_err() {
            let _ = app_clone.emit(
                "conversion-result",
                serde_json::json!({ "success": false, "error": "Finalize error" }),
            );
            return;
        }

        println!("Total packets processed: {}", packet_count);
        let _ = app_clone.emit(
            "progress",
            format!("Conversion complete. Total packets: {}", packet_count),
        );
        let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": true, "message": format!("Converted to mono: {}", output_path.display()) }));
    });

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(CancelFlag(Arc::new(Mutex::new(false))))
        .invoke_handler(tauri::generate_handler![
            convert_to_mono,
            get_audio_info,
            cancel_conversion
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{
        build_output_path, ensure_output_path_allowed, get_audio_info, mono_frame_to_i16,
        normalized_f32_to_i16, render_output_filename, resolve_output_path, ConvertRequest,
        OverwritePolicy, ProgressReader,
    };
    use std::fs;
    use std::io::{Cursor, Read};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};
    use symphonia::core::io::MediaSource;
    use symphonia::core::sample::{i24, u24};

    fn request(file_path: &str) -> ConvertRequest {
        ConvertRequest {
            file_path: file_path.to_string(),
            output_dir: None,
            filename_template: None,
            overwrite_policy: None,
        }
    }

    fn temp_wav_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("monoid-{name}-{nanos}.wav"))
    }

    fn write_test_wav(path: &PathBuf, channels: u16, sample_rate: u32, frames: &[i16]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in frames {
            writer.write_sample(*frame).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn progress_reader_tracks_bytes_read() {
        let bytes_read = Arc::new(Mutex::new(0u64));
        let mut reader = ProgressReader {
            inner: Cursor::new(vec![1_u8, 2, 3, 4, 5]),
            bytes_read: bytes_read.clone(),
            total_size: 5,
        };

        let mut buf = [0_u8; 3];
        let read = reader.read(&mut buf).unwrap();

        assert_eq!(read, 3);
        assert_eq!(buf, [1, 2, 3]);
        assert_eq!(*bytes_read.lock().unwrap(), 3);
        assert_eq!(reader.byte_len(), Some(5));
        assert!(reader.is_seekable());
    }

    #[test]
    fn get_audio_info_reads_wav_metadata() {
        let path = temp_wav_path("audio-info");
        write_test_wav(
            &path,
            2,
            48_000,
            &[1000, -1000, 2000, -2000, 3000, -3000, 4000, -4000],
        );

        let response = get_audio_info(path.to_string_lossy().into_owned());

        fs::remove_file(&path).unwrap();

        assert!(response.success);
        let info = response.data.expect("audio info should be present");
        assert_eq!(info.channels, 2);
        assert_eq!(info.sample_rate, 48_000);
        assert_eq!(info.bits_per_sample, 16);
        assert_eq!(info.duration_seconds, Some(4.0 / 48_000.0));
        assert!(response.error.is_none());
    }

    #[test]
    fn get_audio_info_reports_missing_files() {
        let path = temp_wav_path("missing");

        let response = get_audio_info(path.to_string_lossy().into_owned());

        assert!(!response.success);
        assert!(response.data.is_none());
        assert!(response
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Failed to open file"));
    }

    #[test]
    fn render_output_filename_replaces_supported_tokens() {
        let filename = render_output_filename(
            "{stem}-{original_ext}-mono.{ext}",
            Path::new("/tmp/demo/song.flac"),
        )
        .unwrap();

        assert_eq!(filename, "song-flac-mono.wav");
    }

    #[test]
    fn resolve_output_path_defaults_to_source_directory() {
        let output_path = resolve_output_path(&request("/tmp/demo/song.mp3")).unwrap();

        assert_eq!(output_path, Path::new("/tmp/demo/song_mono.wav"));
    }

    #[test]
    fn resolve_output_path_honors_custom_directory_and_template() {
        let mut request = request("/tmp/demo/song.mp3");
        request.output_dir = Some("/tmp/rendered".to_string());
        request.filename_template = Some("{stem}_mixdown".to_string());

        let output_path = resolve_output_path(&request).unwrap();

        assert_eq!(output_path, Path::new("/tmp/rendered/song_mixdown.wav"));
    }

    #[test]
    fn resolve_output_path_rejects_path_segments_in_template() {
        let mut request = request("/tmp/demo/song.mp3");
        request.filename_template = Some("../escape".to_string());

        let error = resolve_output_path(&request).unwrap_err();

        assert!(error.contains("must not include path separators"));
    }

    #[test]
    fn ensure_output_path_allowed_rejects_existing_file_without_overwrite() {
        let test_dir = std::env::temp_dir().join(format!(
            "monoid-pcu-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&test_dir).unwrap();
        let output_path = test_dir.join("existing.wav");
        fs::write(&output_path, "occupied").unwrap();

        let error = ensure_output_path_allowed(&output_path, OverwritePolicy::Forbid).unwrap_err();
        assert!(error.contains("already exists"));

        fs::remove_dir_all(test_dir).unwrap();
    }

    #[test]
    fn ensure_output_path_allowed_creates_parent_directories() {
        let test_dir = std::env::temp_dir().join(format!(
            "monoid-pcu-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let output_path = test_dir.join("nested/output.wav");

        ensure_output_path_allowed(&output_path, OverwritePolicy::Overwrite).unwrap();

        assert!(output_path.parent().unwrap().exists());

        fs::remove_dir_all(test_dir).unwrap();
    }

    #[test]
    fn output_path_replaces_only_the_final_extension() {
        let output = build_output_path("/tmp/archive.mp3.wav").unwrap();
        assert_eq!(output.to_string_lossy(), "/tmp/archive.mp3_mono.wav");
    }

    #[test]
    fn output_path_does_not_trim_non_extensions() {
        let output = build_output_path("/tmp/draw").unwrap();
        assert_eq!(output.to_string_lossy(), "/tmp/draw_mono.wav");
    }

    #[test]
    fn normalization_clamps_out_of_range_and_non_finite_values() {
        assert_eq!(normalized_f32_to_i16(1.5), i16::MAX);
        assert_eq!(normalized_f32_to_i16(-1.5), i16::MIN);
        assert_eq!(normalized_f32_to_i16(f32::NAN), 0);
    }

    #[test]
    fn mono_frame_supports_24_bit_formats() {
        assert_eq!(mono_frame_to_i16(&[u24::MIN, u24::MAX]), Some(0));
        assert_eq!(mono_frame_to_i16(&[i24::MIN, i24::MAX]), Some(0));
    }

    #[test]
    fn mono_frame_scales_common_formats_consistently() {
        assert_eq!(mono_frame_to_i16(&[u8::MIN]), Some(i16::MIN));
        assert_eq!(mono_frame_to_i16(&[u8::MAX]), Some(32_511));
        assert_eq!(mono_frame_to_i16(&[1.0f32]), Some(i16::MAX));
        assert_eq!(mono_frame_to_i16(&[0.5f32, -0.25f32]), Some(4096));
    }
}

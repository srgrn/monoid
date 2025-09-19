// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
use std::io::{Read, Seek};
use std::sync::{Arc, Mutex};
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

impl<R> symphonia::core::io::MediaSource for ProgressReader<R> where R: Read + Seek + Send + Sync {
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
        let probed = symphonia::default::get_probe().format(&hint, mss, &format_opts, &metadata_opts)
            .map_err(|e| format!("Unsupported format: {}", e))?;

        let format = probed.format;

        let track = format.tracks().iter().find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
            .ok_or("No supported audio tracks")?;

        let channels = track.codec_params.channels.map(|c| c.count() as u32).unwrap_or(0);
        let sample_rate = track.codec_params.sample_rate.ok_or("Unknown sample rate")?;
        let bits_per_sample = track.codec_params.bits_per_sample.unwrap_or(16);

        let duration_seconds = track.codec_params.n_frames.map(|n| n as f64 / sample_rate as f64);

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
fn convert_to_mono(app: tauri::AppHandle, state: tauri::State<CancelFlag>, file_path: String) -> Result<(), String> {
    {
        let mut cancel = state.0.lock().unwrap();
        *cancel = false;
    }
    let cancel_flag = state.0.clone();
    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        println!("Converting file: {}", file_path);
        use std::fs::File;
        use symphonia::core::audio::{AudioBufferRef, Signal};
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
        let progress_reader = ProgressReader { inner: file, bytes_read: bytes_read.clone(), total_size };
        let mss = MediaSourceStream::new(Box::new(progress_reader), Default::default());

        // Probe the format
        let hint = Hint::new();
        let format_opts = FormatOptions::default();
        let metadata_opts = MetadataOptions::default();
        let probed = match symphonia::default::get_probe().format(&hint, mss, &format_opts, &metadata_opts) {
            Ok(p) => p,
            Err(e) => {
                println!("Unsupported format error: {:?}", e);
                let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": format!("Unsupported format: {}", e) }));
                return;
            }
        };

        let mut format = probed.format;

        // Get the default track
        let track = match format.tracks().iter().find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL) {
            Some(t) => t,
            None => {
                let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": "No supported audio tracks" }));
                return;
            }
        };

        let track_id = track.id;
        let sample_rate = match track.codec_params.sample_rate {
            Some(sr) => sr,
            None => {
                let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": "Unknown sample rate" }));
                return;
            }
        };

        // Create a decoder
        let dec_opts = DecoderOptions::default();
        let mut decoder = match symphonia::default::get_codecs().make(&track.codec_params, &dec_opts) {
            Ok(d) => d,
            Err(e) => {
                let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": format!("Unsupported codec: {}", e) }));
                return;
            }
        };

        // Prepare output WAV file with hound
        let output_path = format!("{}_mono.wav", file_path.trim_end_matches(".wav").trim_end_matches(".mp3").trim_end_matches(".flac"));
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
                let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": "Conversion cancelled" }));
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

            // Convert to mono and write as i16
            match decoded {
                AudioBufferRef::U8(buf) => {
                    let buf = buf.as_ref();
                    let channels = buf.spec().channels.count();
                    for i in 0..buf.frames() {
                        let mut sum = 0.0;
                        for ch in 0..channels {
                            sum += buf.chan(ch)[i] as f32 / 128.0 - 1.0;
                        }
                        let mono = (sum / channels as f32 * 32767.0) as i16;
                        if let Err(_) = writer.write_sample(mono) {
                            let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": "Write error" }));
                            return;
                        }
                    }
                }
                AudioBufferRef::U16(buf) => {
                    let buf = buf.as_ref();
                    let channels = buf.spec().channels.count();
                    for i in 0..buf.frames() {
                        let mut sum = 0.0;
                        for ch in 0..channels {
                            sum += buf.chan(ch)[i] as f32 / 32768.0 - 1.0;
                        }
                        let mono = (sum / channels as f32 * 32767.0) as i16;
                        if let Err(_) = writer.write_sample(mono) {
                            let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": "Write error" }));
                            return;
                        }
                    }
                }
                AudioBufferRef::U24(_buf) => {}
                AudioBufferRef::U32(buf) => {
                    let buf = buf.as_ref();
                    let channels = buf.spec().channels.count();
                    for i in 0..buf.frames() {
                        let mut sum = 0.0;
                        for ch in 0..channels {
                            sum += buf.chan(ch)[i] as f32 / 2147483648.0 - 1.0;
                        }
                        let mono = (sum / channels as f32 * 32767.0) as i16;
                        if let Err(_) = writer.write_sample(mono) {
                            let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": "Write error" }));
                            return;
                        }
                    }
                }
                AudioBufferRef::S8(buf) => {
                    let buf = buf.as_ref();
                    let channels = buf.spec().channels.count();
                    for i in 0..buf.frames() {
                        let mut sum = 0.0;
                        for ch in 0..channels {
                            sum += buf.chan(ch)[i] as f32 / 128.0;
                        }
                        let mono = (sum / channels as f32 * 32767.0) as i16;
                        if let Err(_) = writer.write_sample(mono) {
                            let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": "Write error" }));
                            return;
                        }
                    }
                }
                AudioBufferRef::S16(buf) => {
                    let buf = buf.as_ref();
                    let channels = buf.spec().channels.count();
                    for i in 0..buf.frames() {
                        let mut sum = 0.0;
                        for ch in 0..channels {
                            sum += buf.chan(ch)[i] as f32;
                        }
                        let mono = (sum / channels as f32) as i16;
                        if let Err(_) = writer.write_sample(mono) {
                            let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": "Write error" }));
                            return;
                        }
                    }
                }
                AudioBufferRef::S24(_buf) => {}
                AudioBufferRef::S32(buf) => {
                    let buf = buf.as_ref();
                    let channels = buf.spec().channels.count();
                    for i in 0..buf.frames() {
                        let mut sum = 0.0;
                        for ch in 0..channels {
                            sum += buf.chan(ch)[i] as f32 / 32768.0;
                        }
                        let mono = (sum / channels as f32) as i16;
                        if let Err(_) = writer.write_sample(mono) {
                            let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": "Write error" }));
                            return;
                        }
                    }
                }
                AudioBufferRef::F32(buf) => {
                    let buf = buf.as_ref();
                    let channels = buf.spec().channels.count();
                    for i in 0..buf.frames() {
                        let mut sum = 0.0;
                        for ch in 0..channels {
                            sum += buf.chan(ch)[i];
                        }
                        let mono = (sum / channels as f32 * 32767.0) as i16;
                        if let Err(_) = writer.write_sample(mono) {
                            let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": "Write error" }));
                            return;
                        }
                    }
                }
                AudioBufferRef::F64(buf) => {
                    let buf = buf.as_ref();
                    let channels = buf.spec().channels.count();
                    for i in 0..buf.frames() {
                        let mut sum = 0.0;
                        for ch in 0..channels {
                            sum += buf.chan(ch)[i] as f32;
                        }
                        let mono = (sum / channels as f32 * 32767.0) as i16;
                        if let Err(_) = writer.write_sample(mono) {
                            let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": "Write error" }));
                            return;
                        }
                    }
                }
            }
        }

        if writer.finalize().is_err() {
            let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": false, "error": "Finalize error" }));
            return;
        }

        println!("Total packets processed: {}", packet_count);
        let _ = app_clone.emit("progress", format!("Conversion complete. Total packets: {}", packet_count));
        let _ = app_clone.emit("conversion-result", serde_json::json!({ "success": true, "message": format!("Converted to mono: {}", output_path) }));
    });

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(CancelFlag(Arc::new(Mutex::new(false))))
        .invoke_handler(tauri::generate_handler![convert_to_mono, get_audio_info, cancel_conversion])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

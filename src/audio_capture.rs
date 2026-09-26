//! Module capture âm thanh hệ thống (loopback) dùng `cpal`.
//!
//! Trên Windows, `cpal` cho phép mở input stream trên chính thiết bị output
//! mặc định ở chế độ WASAPI loopback — thu lại âm thanh đang phát ra loa/tai
//! nghe, không phải micro. Việc này KHÔNG đụng tới Windows Media Foundation,
//! nên hoạt động được trên cả Windows N/KN/LTSC thiếu Media Feature Pack.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};
use hound::{WavSpec, WavWriter};
use std::fs::File;
use std::io::BufWriter;
use std::sync::{Arc, Mutex};

pub struct AudioCapturer {
    _stream: Stream, // giữ stream sống suốt phiên quay; Drop sẽ dừng capture
}

impl AudioCapturer {
    /// Bắt đầu thu âm thanh loopback, ghi trực tiếp (streaming) ra file WAV tạm.
    pub fn start(wav_path: &str) -> Result<Self> {
        let host = cpal::default_host();

        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("Không tìm thấy thiết bị âm thanh output mặc định"))?;

        let config = device
            .default_output_config()
            .context("Không lấy được cấu hình âm thanh mặc định")?;

        let sample_format = config.sample_format();
        let stream_config: cpal::StreamConfig = config.clone().into();
        let sample_rate = stream_config.sample_rate.0;
        let channels = stream_config.channels;

        let spec = WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let writer = WavWriter::create(wav_path, spec)
            .context("Không tạo được file WAV tạm cho audio")?;
        let writer = Arc::new(Mutex::new(Some(writer)));

        let err_fn = |err| eprintln!("[audio] Lỗi stream: {err}");

        let stream = match sample_format {
            SampleFormat::F32 => {
                let writer = Arc::clone(&writer);
                device.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _| write_samples_f32(&writer, data),
                    err_fn,
                    None,
                )
            }
            SampleFormat::I16 => {
                let writer = Arc::clone(&writer);
                device.build_input_stream(
                    &stream_config,
                    move |data: &[i16], _| {
                        let converted: Vec<f32> =
                            data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                        write_samples_f32(&writer, &converted)
                    },
                    err_fn,
                    None,
                )
            }
            other => {
                return Err(anyhow!("Định dạng sample audio chưa hỗ trợ: {other:?}"));
            }
        }
        .context("Không tạo được audio input stream (loopback)")?;

        stream.play().context("Không thể bắt đầu audio stream")?;

        Ok(Self { _stream: stream })
    }
}

fn write_samples_f32(writer: &Arc<Mutex<Option<WavWriter<BufWriter<File>>>>>, data: &[f32]) {
    if let Ok(mut guard) = writer.lock() {
        if let Some(w) = guard.as_mut() {
            for &sample in data {
                // Bỏ qua lỗi ghi từng sample (ví dụ đĩa đầy) thay vì panic cả audio thread.
                let _ = w.write_sample(sample);
            }
        }
    }
}

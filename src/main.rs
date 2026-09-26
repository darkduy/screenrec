//! screenrec — Công cụ CLI quay video màn hình + âm thanh hệ thống cho Windows,
//! xuất ra file MP4.
//!
//! ## Vì sao kiến trúc này (sau khi đổi hướng)
//! Bản đầu dùng `VideoEncoder` tích hợp sẵn của crate `windows-capture`, nhưng
//! encoder đó dựa vào Windows Media Foundation — component KHÔNG có sẵn trên
//! Windows N/KN và Windows 10/11 LTSC nếu máy chưa cài "Media Feature Pack"
//! (và trên LTSC, capability này nhiều khi không có trong danh mục Windows
//! Update để cài nữa). Điều đó gây lỗi `MF_E_TOPO_CODEC_NOT_FOUND` (0xC00D5212).
//!
//! Để tránh phụ thuộc Media Foundation của máy chạy, kiến trúc mới:
//!   1. `windows_capture` CHỈ dùng để lấy từng khung hình màn hình thô (buffer
//!      BGRA) — phần Graphics Capture API này không liên quan Media Foundation.
//!   2. Từng khung hình thô được ghi thẳng (pipe) vào **FFmpeg** — một tiến
//!      trình ngoài mang theo codec riêng (libx264/AAC), không đụng tới bất kỳ
//!      component nào của Windows. Máy chạy chỉ cần có `ffmpeg.exe` trong PATH.
//!   3. Âm thanh hệ thống (loopback) được thu song song bằng `cpal` (WASAPI,
//!      cũng không phụ thuộc Media Foundation), ghi ra file WAV tạm.
//!   4. Khi dừng quay, gọi FFmpeg một lần nữa để mux WAV vào file MP4 video
//!      vừa tạo ở bước 2, ra file kết quả cuối cùng.
//!
//! Yêu cầu: máy chạy chương trình phải có `ffmpeg.exe` trong PATH.

mod audio_capture;

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;
use windows_capture::capture::{Context as CaptureContext, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use audio_capture::AudioCapturer;

/// Công cụ quay video màn hình + âm thanh hệ thống, xuất ra MP4 (qua FFmpeg).
#[derive(Parser, Debug)]
#[command(name = "screenrec", version, about)]
struct Args {
    /// Đường dẫn file MP4 output cuối cùng.
    #[arg(short, long, default_value = "recording.mp4")]
    output: String,

    /// Chỉ số màn hình cần quay, bắt đầu từ 1.
    #[arg(short, long, default_value_t = 1)]
    monitor: usize,

    /// Số khung hình/giây khi encode video.
    #[arg(short, long, default_value_t = 30)]
    fps: u32,

    /// Bitrate video cho FFmpeg (dùng CRF nên tham số này chỉ mang tính tham khảo
    /// khi muốn ép bitrate cố định qua `--force-bitrate`).
    #[arg(long, default_value_t = 20)]
    crf: u8,

    /// Thời lượng quay tối đa tính bằng giây. Bỏ trống để quay tới khi Ctrl+C.
    #[arg(short, long)]
    duration: Option<u64>,

    /// Tắt thu âm thanh hệ thống.
    #[arg(long, default_value_t = false)]
    no_audio: bool,
}

/// Cấu hình truyền vào handler `Capture` qua cơ chế `Flags` của `windows-capture`.
struct CaptureSettings {
    stop_flag: Arc<AtomicBool>,
    /// stdin của tiến trình FFmpeg đang chạy nền, dùng để pipe frame thô vào.
    ffmpeg_stdin: Arc<Mutex<std::process::ChildStdin>>,
    start_time: Instant,
    max_duration: Option<Duration>,
}

/// Handler nhận frame từ Graphics Capture API và pipe thẳng vào FFmpeg.
struct Capture {
    settings: CaptureSettings,
    frame_count: u64,
    /// Buffer tái sử dụng giữa các frame để `as_nopadding_buffer` không phải
    /// cấp phát bộ nhớ mới mỗi lần gọi — tối ưu hiệu năng khi quay ở fps cao.
    scratch_buffer: Vec<u8>,
}

impl GraphicsCaptureApiHandler for Capture {
    type Flags = CaptureSettings;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: CaptureContext<Self::Flags>) -> Result<Self, Self::Error> {
        println!("Bắt đầu quay. Nhấn Ctrl+C để dừng.");
        Ok(Self {
            settings: ctx.flags,
            frame_count: 0,
            scratch_buffer: Vec::new(),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        self.frame_count += 1;

        // Lấy buffer BGRA thô, KHÔNG padding — đúng định dạng FFmpeg cần khi
        // được khai báo `-f rawvideo -pixel_format bgra`. `as_nopadding_buffer`
        // ghi kết quả vào `scratch_buffer` (tái sử dụng qua từng frame) và trả
        // về `&[u8]` trực tiếp, không phải `Result`.
        let mut buffer = frame.buffer()?;
        let raw = buffer.as_nopadding_buffer(&mut self.scratch_buffer);

        // Ghi thẳng vào stdin của FFmpeg. LƯU Ý: đây là ghi đồng bộ (blocking) —
        // nếu FFmpeg encode chậm hơn tốc độ capture, pipe OS có thể đầy và
        // cuộc gọi `write_all` này sẽ chặn tới khi FFmpeg đọc bớt dữ liệu ra.
        // Với độ phân giải/fps thông thường (1080p @ 30fps trở xuống) trên máy
        // đủ mạnh, FFmpeg với preset "veryfast" thường theo kịp tốc độ capture
        // nên hiếm khi xảy ra. Nếu quay ở độ phân giải/fps rất cao và thấy hiện
        // tượng giật hoặc video bị chậm so với thực tế, cân nhắc hạ fps/bitrate
        // hoặc đổi preset FFmpeg sang "ultrafast".
        let write_result = {
            let mut stdin = self
                .settings
                .ffmpeg_stdin
                .lock()
                .expect("Mutex ffmpeg_stdin bị poison");
            stdin.write_all(raw)
        };
        // Giải phóng borrow của `buffer`/`raw` ngay sau khi dùng xong, trước
        // khi truy cập các field khác của `self` bên dưới — tránh phụ thuộc
        // vào borrow-splitting phức tạp giữa `scratch_buffer` và `settings`.
        drop(buffer);

        if let Err(e) = write_result {
            eprintln!("\nGhi frame vào FFmpeg thất bại (có thể FFmpeg đã thoát): {e}");
            capture_control.stop();
            return Ok(());
        }

        print!(
            "\rĐang quay: {:.1}s ({} khung hình)",
            self.settings.start_time.elapsed().as_secs_f64(),
            self.frame_count
        );
        std::io::stdout().flush().ok();

        let stopped_by_user = self.settings.stop_flag.load(Ordering::SeqCst);
        let reached_max_duration = self
            .settings
            .max_duration
            .is_some_and(|max| self.settings.start_time.elapsed() >= max);

        if stopped_by_user || reached_max_duration {
            println!("\nĐang dừng capture...");
            capture_control.stop();
        }

        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        println!("\nPhiên capture bị đóng đột ngột.");
        self.settings.stop_flag.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// Khởi chạy FFmpeg ở chế độ nhận video thô qua stdin, encode H.264, ghi ra
/// file MP4 tạm (chưa có âm thanh — âm thanh sẽ được mux vào ở bước sau).
fn spawn_ffmpeg_video_only(
    width: u32,
    height: u32,
    fps: u32,
    crf: u8,
    temp_video_path: &str,
) -> Result<Child> {
    let size_arg = format!("{width}x{height}");
    let fps_arg = fps.to_string();
    let crf_arg = crf.to_string();

    Command::new("ffmpeg")
        .args([
            "-y",
            "-f", "rawvideo",
            "-pixel_format", "bgra",
            "-video_size", &size_arg,
            "-framerate", &fps_arg,
            "-i", "-", // đọc video thô từ stdin
            "-c:v", "libx264",
            "-pix_fmt", "yuv420p",
            "-preset", "veryfast",
            "-crf", &crf_arg,
            temp_video_path,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .context(
            "Không chạy được `ffmpeg` — hãy cài FFmpeg (https://ffmpeg.org/download.html) \
             và đảm bảo nó có trong PATH.",
        )
}

/// Mux video (không âm thanh) + audio WAV thành file MP4 cuối cùng.
fn mux_video_and_audio(
    video_only_path: &str,
    wav_path: &str,
    output_path: &str,
) -> Result<()> {
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-i", video_only_path,
            "-i", wav_path,
            "-c:v", "copy", // video đã encode xong ở bước trước, chỉ copy stream
            "-c:a", "aac",
            "-b:a", "192k",
            "-shortest",
            output_path,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("Không chạy được `ffmpeg` để mux audio/video")?;

    if !status.success() {
        bail!("FFmpeg thoát với mã lỗi khi mux: {status}");
    }
    Ok(())
}

fn check_ffmpeg_available() -> Result<()> {
    let result = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match result {
        Ok(status) if status.success() => Ok(()),
        _ => bail!(
            "Không tìm thấy `ffmpeg` trong PATH. Hãy tải FFmpeg tại \
             https://ffmpeg.org/download.html (bản 'essentials' hoặc 'full' \
             cho Windows), giải nén, và thêm thư mục chứa ffmpeg.exe vào PATH."
        ),
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    check_ffmpeg_available()?;

    let monitor = Monitor::from_index(args.monitor).map_err(|_| {
        anyhow::anyhow!(
            "Không tìm thấy màn hình #{}. Hãy kiểm tra lại chỉ số màn hình.",
            args.monitor
        )
    })?;
    let width = monitor
        .width()
        .context("Không lấy được chiều rộng màn hình")?;
    let height = monitor
        .height()
        .context("Không lấy được chiều cao màn hình")?;

    println!("Màn hình #{}: {}x{} @ {} fps", args.monitor, width, height, args.fps);

    // File tạm: video-only (chưa audio) và audio WAV, dọn sau khi mux xong.
    let temp_dir = std::env::temp_dir();
    let temp_video_path = temp_dir
        .join("screenrec_video_only.mp4")
        .to_string_lossy()
        .to_string();
    let wav_path = temp_dir
        .join("screenrec_audio.wav")
        .to_string_lossy()
        .to_string();

    // Bắt đầu thu âm thanh song song (nếu không tắt).
    let audio_capturer = if args.no_audio {
        None
    } else {
        println!("Đang thu âm thanh hệ thống (loopback)...");
        Some(AudioCapturer::start(&wav_path).context("Khởi tạo audio capturer thất bại")?)
    };

    // Khởi chạy FFmpeg nhận video thô qua stdin.
    let mut ffmpeg_process =
        spawn_ffmpeg_video_only(width, height, args.fps, args.crf, &temp_video_path)?;
    let ffmpeg_stdin = ffmpeg_process
        .stdin
        .take()
        .context("Không lấy được stdin của tiến trình FFmpeg")?;
    let ffmpeg_stdin = Arc::new(Mutex::new(ffmpeg_stdin));

    let stop_flag = Arc::new(AtomicBool::new(false));
    {
        let stop_flag = Arc::clone(&stop_flag);
        ctrlc::set_handler(move || {
            stop_flag.store(true, Ordering::SeqCst);
        })
        .context("Không đăng ký được trình xử lý Ctrl+C")?;
    }

    let capture_settings = CaptureSettings {
        stop_flag,
        ffmpeg_stdin: Arc::clone(&ffmpeg_stdin),
        start_time: Instant::now(),
        max_duration: args.duration.map(Duration::from_secs),
    };

    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::Default,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        capture_settings,
    );

    // `Capture::start` chiếm thread hiện tại tới khi `capture_control.stop()`
    // được gọi bên trong `on_frame_arrived`/`on_closed`.
    if let Err(e) = Capture::start(settings) {
        eprintln!("Quay màn hình thất bại: {e}");
        // Vẫn cố dọn tiến trình FFmpeg trước khi thoát để không để lại process rác.
        let _ = ffmpeg_process.kill();
        std::process::exit(1);
    }

    // Đóng stdin để FFmpeg biết luồng video đã kết thúc và flush/finalize file.
    // Dùng try_unwrap thay vì unwrap trực tiếp: nếu vì lý do nào đó vẫn còn
    // tham chiếu Arc khác (không nên xảy ra sau khi Capture::start đã return
    // và handler bên trong đã bị drop, nhưng ta không đặt cược an toàn của
    // chương trình vào giả định đó), ta chủ động drop toàn bộ Arc thay vì
    // panic — khi đó FFmpeg vẫn sẽ tự đóng khi tiến trình `screenrec` thoát,
    // hoặc `wait()` bên dưới có thể bị treo nếu FFmpeg chờ EOF vô thời hạn.
    match Arc::try_unwrap(ffmpeg_stdin) {
        Ok(mutex) => drop(mutex.into_inner().expect("Mutex ffmpeg_stdin bị poison")),
        Err(arc) => {
            eprintln!(
                "Cảnh báo: vẫn còn tham chiếu khác tới stdin của FFmpeg, \
                 có thể chương trình chờ FFmpeg lâu hơn dự kiến."
            );
            drop(arc);
        }
    }

    println!("Đang chờ FFmpeg encode xong video...");
    let status = ffmpeg_process
        .wait()
        .context("Chờ tiến trình FFmpeg kết thúc thất bại")?;
    if !status.success() {
        bail!("FFmpeg (encode video) thoát với mã lỗi: {status}");
    }

    // Dừng thu âm thanh (nếu có) trước khi mux, đảm bảo WAV đã ghi đầy đủ.
    drop(audio_capturer);
    std::thread::sleep(Duration::from_millis(300));

    if args.no_audio {
        // Không có audio để mux — đổi tên file video-only thành output cuối cùng.
        std::fs::rename(&temp_video_path, &args.output)
            .context("Không đổi tên file video thành output cuối cùng")?;
    } else {
        println!("Đang ghép âm thanh vào video...");
        mux_video_and_audio(&temp_video_path, &wav_path, &args.output)?;
        let _ = std::fs::remove_file(&temp_video_path);
        let _ = std::fs::remove_file(&wav_path);
    }

    println!("Hoàn tất! Video đã lưu tại: {}", args.output);
    Ok(())
}

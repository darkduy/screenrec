//! screenrec — Công cụ CLI quay video màn hình + âm thanh hệ thống cho Windows,
//! xuất trực tiếp ra file MP4.
//!
//! ## Kiến trúc
//! Dùng crate `windows-capture`, thư viện bọc an toàn (safe wrapper) quanh:
//!   - Windows Graphics Capture API (và Desktop Duplication API làm nền) để lấy
//!     từng khung hình màn hình hiệu năng cao, chỉ cập nhật khi có thay đổi.
//!   - Windows Media Foundation video encoder tích hợp sẵn, hỗ trợ mã hoá
//!     H.264 phần cứng + audio loopback (âm thanh hệ thống) và mux thẳng ra MP4.
//!
//! Nhờ vậy KHÔNG cần cài đặt hay gọi FFmpeg như một tiến trình ngoài — toàn bộ
//! pipeline capture -> encode -> mux nằm gọn trong một crate Rust duy nhất,
//! giảm bề mặt lỗi và đơn giản hoá triển khai/phân phối cho người dùng cuối.
//!
//! ## Luồng chạy
//! 1. Parse tham số CLI (output path, fps, bitrate, monitor index, duration).
//! 2. Đăng ký handler Ctrl+C để đặt cờ dừng dùng chung.
//! 3. Tạo `Settings` cho phiên capture (màn hình mục tiêu, cursor, color format...).
//! 4. `Capture::start(settings)` chiếm quyền điều khiển thread hiện tại; bên trong,
//!    `GraphicsCaptureApiHandler` của ta (`Capture`) nhận từng frame qua
//!    `on_frame_arrived`, đẩy vào `VideoEncoder`, và tự dừng khi cờ `stop_flag`
//!    được bật hoặc khi đạt thời lượng tối đa yêu cầu.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::encoder::{
    AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

/// Công cụ quay video màn hình + âm thanh hệ thống, xuất ra MP4.
#[derive(Parser, Debug)]
#[command(name = "screenrec", version, about)]
struct Args {
    /// Đường dẫn file MP4 output.
    #[arg(short, long, default_value = "recording.mp4")]
    output: String,

    /// Chỉ số màn hình cần quay, bắt đầu từ 1 (dùng khi có nhiều màn hình).
    #[arg(short, long, default_value_t = 1)]
    monitor: usize,

    /// Số khung hình/giây khi encode video.
    #[arg(short, long, default_value_t = 30)]
    fps: u32,

    /// Bitrate video, đơn vị bit/giây. Mặc định 8 Mbps — cân bằng tốt giữa
    /// chất lượng hình ảnh và dung lượng file cho video màn hình.
    #[arg(short, long, default_value_t = 8_000_000)]
    bitrate: u32,

    /// Thời lượng quay tối đa tính bằng giây. Bỏ trống để quay đến khi nhấn Ctrl+C.
    #[arg(short, long)]
    duration: Option<u64>,

    /// Tắt thu âm thanh hệ thống (mặc định: có thu âm thanh loopback).
    #[arg(long, default_value_t = false)]
    no_audio: bool,
}

/// Cấu hình được truyền vào handler `Capture` thông qua cơ chế `Flags` của
/// `windows-capture`. Đây là cách thư viện cho phép "tiêm" dữ liệu tuỳ biến
/// vào handler tại thời điểm khởi tạo (`Capture::new`).
struct CaptureSettings {
    /// Cờ dùng chung với Ctrl+C handler để báo hiệu dừng quay.
    stop_flag: Arc<AtomicBool>,
    output_path: String,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
    audio_enabled: bool,
    /// Thời điểm bắt đầu quay, dùng để tính đã quay được bao lâu.
    start_time: Instant,
    /// Thời lượng tối đa (nếu có); None nghĩa là quay vô thời hạn tới khi Ctrl+C.
    max_duration: Option<Duration>,
}

/// Handler xử lý sự kiện capture: nhận frame, đẩy vào encoder, và quyết định
/// khi nào dừng phiên quay.
struct Capture {
    encoder: Option<VideoEncoder>,
    settings: CaptureSettings,
    frame_count: u64,
}

impl GraphicsCaptureApiHandler for Capture {
    type Flags = CaptureSettings;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    /// Được gọi một lần khi phiên capture khởi tạo. Tạo video encoder ở đây vì
    /// đã biết trước kích thước khung hình từ `settings`.
    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let settings = ctx.flags;

        let video_settings = VideoSettingsBuilder::new(settings.width, settings.height)
            .bitrate(settings.bitrate)
            .frame_rate(settings.fps);

        // AudioSettingsBuilder mặc định BẬT thu âm thanh loopback (âm thanh hệ
        // thống đang phát ra loa/tai nghe). Chỉ khi người dùng truyền `--no-audio`
        // ta mới tắt đi bằng `.disabled(true)`.
        let audio_settings = AudioSettingsBuilder::default().disabled(!settings.audio_enabled);

        let encoder = VideoEncoder::new(
            video_settings,
            audio_settings,
            ContainerSettingsBuilder::default(),
            &settings.output_path,
        )?;

        println!(
            "Bắt đầu quay: {}x{} @ {} fps, bitrate {} bps, âm thanh: {}",
            settings.width,
            settings.height,
            settings.fps,
            settings.bitrate,
            if settings.audio_enabled { "có" } else { "không" }
        );
        println!("Nhấn Ctrl+C để dừng quay.");

        Ok(Self {
            encoder: Some(encoder),
            settings,
            frame_count: 0,
        })
    }

    /// Được gọi mỗi khi có một khung hình mới từ Graphics Capture API.
    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        self.frame_count += 1;

        print!(
            "\rĐang quay: {:.1}s ({} khung hình)",
            self.settings.start_time.elapsed().as_secs_f64(),
            self.frame_count
        );
        io::stdout().flush()?;

        // Đẩy frame vào encoder để mã hoá và ghi vào file MP4.
        self.encoder
            .as_mut()
            .expect("Encoder không được None trong khi vẫn đang quay")
            .send_frame(frame)?;

        // Điều kiện dừng 1: người dùng nhấn Ctrl+C (stop_flag được set từ main thread).
        let stopped_by_user = self.settings.stop_flag.load(Ordering::SeqCst);

        // Điều kiện dừng 2: đã đạt thời lượng tối đa được yêu cầu (nếu có).
        let reached_max_duration = self
            .settings
            .max_duration
            .is_some_and(|max| self.settings.start_time.elapsed() >= max);

        if stopped_by_user || reached_max_duration {
            println!("\nĐang hoàn tất và lưu file MP4...");

            // `finish()` sẽ flush toàn bộ dữ liệu còn lại và đóng file đúng cách.
            // Nếu không gọi bước này, file MP4 có thể bị hỏng hoặc thiếu dữ liệu cuối.
            self.encoder
                .take()
                .expect("Encoder không được None khi finalize")
                .finish()?;

            capture_control.stop();
            println!("Hoàn tất!");
        }

        Ok(())
    }

    /// Được gọi nếu màn hình/cửa sổ bị capture đóng lại giữa chừng (ví dụ tắt máy,
    /// ngắt kết nối màn hình). Đảm bảo dừng vòng lặp một cách an toàn.
    fn on_closed(&mut self) -> Result<(), Self::Error> {
        println!("\nPhiên capture bị đóng đột ngột (có thể do màn hình bị ngắt kết nối).");
        self.settings.stop_flag.store(true, Ordering::SeqCst);
        Ok(())
    }
}

fn main() {
    let args = Args::parse();

    // Xác định màn hình cần quay theo chỉ số người dùng chọn (1-based, khớp
    // quy ước hiển thị số màn hình trong Windows Display Settings).
    let monitor = Monitor::from_index(args.monitor).unwrap_or_else(|_| {
        eprintln!(
            "Lỗi: không tìm thấy màn hình #{}. Hãy kiểm tra lại chỉ số màn hình.",
            args.monitor
        );
        std::process::exit(1);
    });

    let width = monitor
        .width()
        .expect("Không lấy được chiều rộng màn hình");
    let height = monitor
        .height()
        .expect("Không lấy được chiều cao màn hình");

    // Cờ dừng dùng chung giữa Ctrl+C handler và vòng lặp capture bên trong thư viện.
    let stop_flag = Arc::new(AtomicBool::new(false));
    {
        let stop_flag = Arc::clone(&stop_flag);
        ctrlc::set_handler(move || {
            stop_flag.store(true, Ordering::SeqCst);
        })
        .expect("Không đăng ký được trình xử lý Ctrl+C");
    }

    let capture_settings = CaptureSettings {
        stop_flag,
        output_path: args.output.clone(),
        width,
        height,
        fps: args.fps,
        bitrate: args.bitrate,
        audio_enabled: !args.no_audio,
        start_time: Instant::now(),
        max_duration: args.duration.map(Duration::from_secs),
    };

    let settings = Settings::new(
        monitor,
        // Hiển thị con trỏ chuột trong video — hành vi mong đợi cho hầu hết
        // nhu cầu quay màn hình (hướng dẫn, demo...).
        CursorCaptureSettings::Default,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        // BGRA8: định dạng phổ biến nhất, tương thích tốt với encoder.
        ColorFormat::Bgra8,
        capture_settings,
    );

    // `Capture::start` chiếm quyền điều khiển thread hiện tại cho tới khi
    // `capture_control.stop()` được gọi bên trong `on_frame_arrived`.
    if let Err(e) = Capture::start(settings) {
        eprintln!("Quay màn hình thất bại: {e}");
        std::process::exit(1);
    }

    println!("Video đã lưu tại: {}", args.output);
}

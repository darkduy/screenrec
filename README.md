# screenrec

Công cụ CLI đơn giản quay video màn hình + âm thanh hệ thống trên Windows,
xuất ra file MP4.

## Kiến trúc

- **Video**: dùng crate [`windows-capture`](https://docs.rs/windows-capture)
  để lấy từng khung hình màn hình thô qua Windows Graphics Capture API, sau đó
  pipe trực tiếp vào **FFmpeg** để encode H.264.
- **Audio**: dùng `cpal` (WASAPI loopback) để thu âm thanh hệ thống, ghi ra
  file WAV tạm.
- Khi dừng quay, gọi FFmpeg một lần nữa để mux audio WAV vào video đã encode,
  ra file MP4 cuối cùng.

### Vì sao không dùng encoder tích hợp sẵn của `windows-capture`

Bản đầu tiên dùng `VideoEncoder` tích hợp sẵn của thư viện (dựa trên Windows
Media Foundation). Cách đó gọn hơn (không cần cài FFmpeg), nhưng **thất bại
trên Windows N/KN và Windows 10/11 LTSC** với lỗi:

```
Windows API error: No suitable transform was found to encode or decode
the content. (0xC00D5212)
```

Nguyên nhân: các bản Windows đó không có sẵn Media Foundation transforms cho
H.264/AAC, và trên LTSC, "Media Feature Pack" nhiều khi **không có** trong
danh mục Windows Update để cài bù (`DISM /Add-Capability` báo lỗi 87 hoặc
1168 — "Element not found").

Chuyển sang dùng FFmpeg (mang theo codec riêng, không phụ thuộc Windows) giải
quyết dứt điểm vấn đề này, đổi lại người dùng cần cài FFmpeg riêng.

## Yêu cầu hệ thống

- Windows 10 (1903+) hoặc Windows 11 — **bao gồm cả bản N/KN/LTSC**.
- **FFmpeg** cài sẵn và có trong PATH. Tải tại
  [ffmpeg.org/download.html](https://ffmpeg.org/download.html) (bản
  "essentials" hoặc "full" cho Windows), giải nén, thêm thư mục chứa
  `ffmpeg.exe` vào biến môi trường PATH.
- Rust toolchain (chỉ cần khi build từ mã nguồn), target
  `x86_64-pc-windows-msvc`.

## Build

### Cách 1: GitHub Actions

Repo có sẵn workflow tại `.github/workflows/build.yml`, chạy trên
`windows-latest`. Sau khi push code lên GitHub, vào tab **Actions** → chọn
workflow **Build screenrec.exe** → tải artifact `screenrec-windows-x64` sau
khi chạy xong.

Lưu ý: workflow chỉ **build** file `.exe` — không cần FFmpeg trong lúc build.
FFmpeg chỉ cần có mặt trên máy **chạy** `screenrec.exe` sau này.

### Cách 2: Build cục bộ

```powershell
cargo build --release
```

File thực thi nằm tại `target\release\screenrec.exe`.

## Sử dụng

```powershell
# Quay màn hình chính, có âm thanh, tới khi Ctrl+C
screenrec.exe

# Quay màn hình thứ 2, fps 60, chất lượng cao hơn (CRF thấp hơn = nét hơn)
screenrec.exe --monitor 2 --fps 60 --crf 18 --output demo.mp4

# Quay đúng 30 giây rồi tự dừng, không kèm âm thanh
screenrec.exe --duration 30 --no-audio
```

### Tham số

| Tham số        | Mặc định        | Mô tả                                              |
|----------------|-----------------|-----------------------------------------------------|
| `-o, --output` | `recording.mp4` | Đường dẫn file MP4 output                           |
| `-m, --monitor`| `1`             | Chỉ số màn hình cần quay (1 = màn hình chính)       |
| `-f, --fps`    | `30`            | Số khung hình/giây                                   |
| `--crf`        | `20`            | Chất lượng video H.264 (0–51, càng thấp càng nét, file càng lớn) |
| `-d, --duration`| (không giới hạn)| Thời lượng quay tối đa, tính bằng giây             |
| `--no-audio`   | tắt             | Truyền cờ này để **không** thu âm thanh hệ thống    |

Nhấn **Ctrl+C** để dừng quay; chương trình sẽ tự encode và mux file MP4 hoàn
chỉnh trước khi thoát.

## Giới hạn đã biết

- Chỉ thu được **âm thanh hệ thống (loopback)**, không thu micro.
- Ghi frame vào FFmpeg qua pipe là thao tác đồng bộ (blocking). Với độ phân
  giải/fps rất cao trên máy yếu, nếu FFmpeg encode không kịp tốc độ capture,
  chương trình có thể bị khựng lại chờ FFmpeg xử lý. Cách giảm thiểu: hạ fps,
  tăng `--crf`, hoặc tự đổi preset FFmpeg trong mã nguồn (`src/main.rs`, hàm
  `spawn_ffmpeg_video_only`) từ `veryfast` sang `ultrafast`.
- Cần chạy trên phiên desktop tương tác thông thường; một số cấu hình Remote
  Desktop (RDP) có thể không hỗ trợ đầy đủ Graphics Capture API.

## Hướng cải tiến tiếp theo

1. **Ghi frame vào FFmpeg qua thread/queue riêng** thay vì ghi đồng bộ ngay
   trong `on_frame_arrived`, để capture không bị chặn khi encode chậm — đánh
   đổi lấy độ phức tạp code cao hơn và cần quản lý bộ nhớ đệm (frame drop khi
   queue đầy).
2. **Thu thêm micro** và mix hai nguồn âm thanh trước khi đưa vào WAV.
3. **Chọn vùng quay tuỳ ý (crop)** thay vì luôn quay nguyên màn hình.
4. **GUI tối giản** (ví dụ `egui`) với nút Start/Stop thay vì thuần CLI.
5. **Tự động đặt tên file theo timestamp** khi không chỉ định `--output`.
6. **Kiểm tra dung lượng ổ đĩa còn trống** trước khi quay dài.

# screenrec

Công cụ CLI đơn giản quay video màn hình + âm thanh hệ thống trên Windows,
xuất trực tiếp ra file MP4.

## Vì sao thiết kế thế này

Dùng crate [`windows-capture`](https://docs.rs/windows-capture), một thư viện
Rust bọc an toàn quanh:

- **Windows Graphics Capture API** (dựa trên Desktop Duplication API) để lấy
  từng khung hình màn hình hiệu năng cao.
- **Windows Media Foundation encoder** tích hợp sẵn, hỗ trợ mã hoá H.264 phần
  cứng kèm audio loopback (âm thanh hệ thống) và mux thẳng ra MP4.

**Không cần cài FFmpeg hay bất kỳ phần mềm ngoài nào** — chỉ cần build và chạy
một file `.exe` duy nhất trên Windows 10/11.

## Yêu cầu hệ thống

- Windows 10 (bản 1903+) hoặc Windows 11.
- Rust toolchain (cài qua [rustup](https://rustup.rs/)), target
  `x86_64-pc-windows-msvc` (mặc định khi cài rustup trên Windows).

## Build

```powershell
cargo build --release
```

File thực thi sau khi build nằm tại `target\release\screenrec.exe`.

## Sử dụng

```powershell
# Quay màn hình chính (monitor 1), có âm thanh, tới khi nhấn Ctrl+C
screenrec.exe

# Quay màn hình thứ 2, fps 60, bitrate 15 Mbps, lưu ra "demo.mp4"
screenrec.exe --monitor 2 --fps 60 --bitrate 15000000 --output demo.mp4

# Quay đúng 30 giây rồi tự dừng
screenrec.exe --duration 30

# Quay không kèm âm thanh
screenrec.exe --no-audio
```

### Tham số

| Tham số        | Mặc định        | Mô tả                                              |
|----------------|-----------------|-----------------------------------------------------|
| `-o, --output` | `recording.mp4` | Đường dẫn file MP4 output                           |
| `-m, --monitor`| `1`             | Chỉ số màn hình cần quay (1 = màn hình chính)       |
| `-f, --fps`    | `30`            | Số khung hình/giây                                   |
| `-b, --bitrate`| `8000000`       | Bitrate video (bit/giây)                             |
| `-d, --duration`| (không giới hạn)| Thời lượng quay tối đa, tính bằng giây             |
| `--no-audio`   | tắt             | Truyền cờ này để **không** thu âm thanh hệ thống    |

Nhấn **Ctrl+C** bất kỳ lúc nào để dừng quay và lưu file MP4 hoàn chỉnh.

## Giới hạn đã biết

- Chỉ quay được **âm thanh hệ thống (loopback)** — không thu micro. Nếu cần
  thêm micro, đây là hướng mở rộng tự nhiên tiếp theo (xem phần dưới).
- Cần chạy trên phiên desktop tương tác thông thường; một số cấu hình chạy qua
  Remote Desktop (RDP) có thể không hỗ trợ Graphics Capture API đầy đủ.

## Hướng cải tiến tiếp theo

1. **Thu thêm micro**: mở thêm một audio stream từ thiết bị input mặc định
   (qua chính `windows-capture` nếu hỗ trợ, hoặc `cpal`), rồi mix hai nguồn
   âm thanh trước khi đưa vào encoder.
2. **Chọn vùng quay tuỳ ý (crop)**: hiện quay nguyên màn hình; có thể thêm
   tham số `--region x,y,w,h` và crop buffer trước khi gửi vào encoder.
3. **GUI tối giản**: bọc logic hiện có bằng một cửa sổ nhỏ (ví dụ dùng `egui`)
   với nút Start/Stop, thay vì thuần CLI.
4. **Tự động đặt tên file theo timestamp** khi người dùng không chỉ định
   `--output`, tránh ghi đè file cũ.
5. **Kiểm tra dung lượng ổ đĩa còn trống** trước khi bắt đầu quay dài, cảnh
   báo sớm thay vì để quay lỡ dở.

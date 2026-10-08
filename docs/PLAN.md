# Kế hoạch phát triển OwlMap

> Cập nhật: 08/10/2026 · Chủ dự án: Ninh Lee

## Tiến độ

- [x] Landing page lên Vercel: https://owlmap-ninh-le-projects.vercel.app
- [x] Lõi phân tích dạng CLI (Ruby, `cli/`): clone repo → lọc file → chia module → Claude tóm tắt → viết ARCHITECTURE / FLOWS / ONBOARDING / modules. 18 test offline đều qua; dry-run trên Sinatra: 219 file, 10 module, ~188k token đầu vào.
- [ ] Chạy thật lần đầu với API key, chấm chất lượng trên 3–5 repo, tinh chỉnh prompt
- [ ] Web MVP (tuần 4)

## 1. Tóm tắt

**OwlMap** biến một codebase thiếu tài liệu thành bản đồ dễ hiểu: tổng quan kiến trúc, sơ đồ luồng chính, ghi chú từng module và hướng dẫn onboarding — sinh tự động bằng Claude.

- **Vấn đề:** dự án nào cũng có phần code không ai viết tài liệu. Dev mới mất hàng tuần để hiểu hệ thống; người cũ nghỉ việc thì kiến thức đi theo.
- **Người dùng mục tiêu:** tech lead và dev ở các team vừa và nhỏ, công ty outsource/offshore (đặc biệt team làm dự án Nhật), agency nhận bàn giao dự án cũ.
- **Vai trò của Claude:** đọc và hiểu lượng code lớn, suy luận kiến trúc, viết tài liệu và sơ đồ. Bỏ Claude đi thì sản phẩm không chạy được.
- **Chỉ số thành công của MVP:** thời gian một dev mới trả lời được 5 câu hỏi cơ bản về dự án (luồng đăng nhập ở đâu, dữ liệu lưu thế nào…) giảm rõ rệt khi có tài liệu OwlMap.

## 2. Phạm vi MVP

**Làm:**
1. Nhập link repo GitHub **public** (giới hạn ~500 file mã nguồn).
2. Lọc file không cần thiết (`node_modules`, `vendor`, build, ảnh, lockfile…).
3. Claude phân tích theo hai tầng: tóm tắt từng module → tổng hợp kiến trúc toàn hệ thống.
4. Sinh bộ tài liệu Markdown:
   - `ARCHITECTURE.md` — tổng quan, các thành phần, luồng dữ liệu
   - `FLOWS.md` — 2–4 luồng chính kèm sơ đồ Mermaid
   - `modules/*.md` — mỗi module: làm gì, phụ thuộc gì, chỗ nào rủi ro khi sửa
   - `ONBOARDING.md` — đọc gì trước, chạy dự án thế nào, câu hỏi thường gặp
5. Xem tài liệu trên web (render Markdown + Mermaid) và tải về file `.zip`.

**Chưa làm (để sau MVP):** repo private (GitHub OAuth/App), tự cập nhật khi có commit mới, chat hỏi đáp về codebase, tài khoản người dùng, thanh toán, song ngữ Nhật–Việt.

## 3. Kiến trúc kỹ thuật đề xuất

```
[Web UI] --repo URL--> [API server] --clone (shallow)--> [Thư mục tạm]
                            |
                            +--> Bộ lọc & chia nhỏ file theo module
                            +--> Claude API: tóm tắt từng module (song song)
                            +--> Claude API: tổng hợp kiến trúc + luồng + onboarding
                            +--> Ghi Markdown, nén .zip
[Web UI] <--kết quả + trạng thái job-- [API server]
```

| Thành phần | Lựa chọn gợi ý | Ghi chú |
|---|---|---|
| Backend | Ruby (đã chọn) — lõi CLI thuần thư viện chuẩn; web MVP dự kiến Rails | Chọn stack bạn quen nhất để làm nhanh |
| Hàng đợi job | Sidekiq / BullMQ | Phân tích repo mất vài phút, không chạy trong request |
| Frontend | Trang tĩnh + JS, hoặc Next.js | Render Markdown (marked) + Mermaid |
| AI | Claude API (Claude Console) | Dùng model mạnh cho bước tổng hợp, model nhanh/rẻ cho bước tóm tắt module |
| Deploy | Render / Railway / Fly.io | Landing page tĩnh: GitHub Pages hoặc Vercel |

**Điểm kỹ thuật cần lưu ý**
- **Chi phí API:** ước tính token trước khi chạy, đặt trần cho mỗi repo; dùng prompt caching cho phần chỉ dẫn lặp lại.
- **Repo lớn:** tóm tắt nhiều tầng (file → module → hệ thống) thay vì nhồi toàn bộ code vào một lần gọi.
- **Bảo mật:** chỉ repo public ở MVP; xoá thư mục clone sau khi xử lý; không log nội dung code.
- **Chất lượng:** lưu bộ 5–10 repo open-source làm "bộ kiểm thử", chấm output sau mỗi lần sửa prompt.

## 4. Lộ trình (làm ngoài giờ, ~10–15 giờ/tuần)

| Tuần | Mục tiêu | Đầu ra |
|---|---|---|
| 1 | Chuẩn bị | Kiểm tra hợp đồng lao động; mua tên miền; email theo tên miền; tài khoản Claude Console; landing page lên mạng với form đăng ký chờ |
| 2 | Lõi phân tích | Script CLI: nhập repo → sinh Markdown. Thử trên 3 repo open-source |
| 3 | Chất lượng | Tinh chỉnh prompt, sơ đồ Mermaid, xử lý repo lớn; bộ kiểm thử 5–10 repo |
| 4 | Web MVP | Giao diện nhập link, theo dõi tiến độ, xem và tải tài liệu; deploy |
| 5 | Người dùng thử | Mời 5–10 dev dùng trên repo thật của họ; đo thời gian và thu phản hồi |
| 6 | Nộp đơn | Video demo 1–2 phút; cập nhật landing với kết quả thử nghiệm; nộp Claude for Startups |

Song song từ tuần 3–6: chuẩn bị và nộp hồ sơ thành lập công ty (mục 6).

## 5. Chi phí ước tính giai đoạn đầu

| Hạng mục | Ước tính |
|---|---|
| Tên miền (`.dev` / `.io.vn` / `.com`) | ~10–80 USD/năm tuỳ đuôi |
| Email theo tên miền (Zoho Mail free / Google Workspace) | 0–7 USD/tháng |
| Claude API trong giai đoạn thử nghiệm | Trả theo mức dùng; đặt giới hạn chi tiêu trong Console |
| Hosting | 0–10 USD/tháng (gói miễn phí/thấp nhất) |
| Thành lập công ty (nếu tự làm online) | Lệ phí nhà nước thấp; thêm chữ ký số, con dấu, ngân hàng (xem mục 6) |

## 6. Đăng ký doanh nghiệp tại Việt Nam

> Đây là hướng dẫn tham khảo, không phải tư vấn pháp lý. Quy định thay đổi thường xuyên — hãy kiểm tra lại trên Cổng thông tin quốc gia về đăng ký doanh nghiệp (dangkykinhdoanh.gov.vn) hoặc hỏi một đơn vị kế toán/luật trước khi nộp.

### 6.1 Chọn loại hình
- **Công ty TNHH một thành viên** — phù hợp nhất khi bạn là người sáng lập duy nhất: trách nhiệm hữu hạn trong phạm vi vốn góp, dễ thêm thành viên/nhà đầu tư sau này (chuyển đổi loại hình).
- **Hộ kinh doanh** — thủ tục đơn giản hơn, nhưng **không phải là công ty**; không phù hợp nếu mục tiêu là chương trình startup, gọi vốn hay ký hợp đồng với khách nước ngoài.

### 6.2 Trước khi nộp
- [ ] Đọc hợp đồng lao động hiện tại: điều khoản sở hữu trí tuệ, làm thêm, cấm cạnh tranh, có phải báo cáo khi tham gia góp vốn/quản lý doanh nghiệp khác không.
- [ ] Chọn tên công ty và tra trùng trên dangkykinhdoanh.gov.vn (ví dụ: "Công ty TNHH OwlMap").
- [ ] Chọn **ngành nghề** (mã VSIC), ví dụ: 6201 Lập trình máy vi tính; 6202 Tư vấn máy vi tính và quản trị hệ thống máy vi tính; 6311 Xử lý dữ liệu, cho thuê và các hoạt động liên quan.
- [ ] Địa chỉ trụ sở hợp lệ (lưu ý quy định hạn chế đặt trụ sở tại căn hộ chung cư dùng để ở).
- [ ] Mức vốn điều lệ (không có mức tối thiểu cho các ngành trên; phải góp đủ trong 90 ngày).

### 6.3 Hồ sơ
- Giấy đề nghị đăng ký doanh nghiệp
- Điều lệ công ty
- Bản sao giấy tờ pháp lý cá nhân (CCCD) của chủ sở hữu/người đại diện theo pháp luật

### 6.4 Nộp hồ sơ
- Nộp online qua **dangkykinhdoanh.gov.vn** (dùng tài khoản định danh điện tử VNeID hoặc chữ ký số), hoặc trực tiếp tại Phòng Đăng ký kinh doanh. Từ 01/07/2025, thủ tục theo **Nghị định 168/2025/NĐ-CP** (thay Nghị định 01/2021).
- Thời gian xử lý: thường **3 ngày làm việc** kể từ khi hồ sơ hợp lệ.
- Lệ phí đăng ký và phí công bố thông tin: mức thấp (khoảng vài chục đến 100 nghìn đồng/lần) — kiểm tra mức hiện hành khi nộp.

### 6.5 Sau khi có Giấy chứng nhận đăng ký doanh nghiệp
- [ ] Công bố nội dung đăng ký doanh nghiệp
- [ ] Khắc dấu (nếu dùng) và thông báo mẫu dấu nếu cần
- [ ] Mua chữ ký số (token/remote signing) để khai thuế, hoá đơn
- [ ] Mở tài khoản ngân hàng doanh nghiệp, thông báo với cơ quan thuế
- [ ] Đăng ký hoá đơn điện tử
- [ ] Treo biển hiệu tại trụ sở
- [ ] Thuê dịch vụ kế toán (thường vài trăm nghìn đến ~1 triệu đồng/tháng cho công ty nhỏ chưa phát sinh nhiều)
- **Lệ phí môn bài:** đã chấm dứt thu từ 01/01/2026 (Nghị quyết 198/2025/QH15, Nghị định 362/2025/NĐ-CP).

### 6.6 Đồng bộ với chương trình Claude for Startups
- Tên miền và email nên đứng tên công ty (chuyển chủ thể tên miền sau khi thành lập).
- Dùng email theo tên miền khi tạo tài khoản Claude Console của công ty.

## 7. Nộp đơn Claude for Startups

**Điều kiện:** startup thành lập trong 5 năm gần nhất hoặc gọi vốn trong 2 năm; có tài khoản Claude Console; email công ty khớp tên miền website; mô tả ngắn sản phẩm. Không bắt buộc có vốn VC.

**Quyền lợi (theo trang chương trình, 10/2026):** 1 năm Claude Team miễn phí (tối đa 5 Premium seat, chỉ cho tổ chức chưa từng dùng Team), 1.000 USD API credits (hết hạn sau 6 tháng), ưu đãi đối tác, office hours, sự kiện.

**Checklist trước khi nộp**
- [ ] Landing page có nội dung thật, demo video, mô tả rõ vai trò của Claude
- [ ] Email `@<tên-miền>` hoạt động
- [ ] Có MVP chạy được và kết quả thử nghiệm với người dùng thật
- [ ] Mô tả sản phẩm (tiếng Anh), ví dụ:

> OwlMap turns undocumented codebases into a navigable map. Claude reads the repository, then writes an architecture overview, flow diagrams, per-module notes and an onboarding guide. In a pilot with N developers, time to answer basic questions about an unfamiliar project dropped from X to Y.

*(Thay N, X, Y bằng số liệu thật.)*

## 8. Rủi ro và cách xử lý

| Rủi ro | Cách xử lý |
|---|---|
| Xung đột với hợp đồng lao động hiện tại | Đọc kỹ hợp đồng trước khi bắt đầu; làm ngoài giờ, trên thiết bị và tài khoản cá nhân; không dùng code/dữ liệu của công ty |
| Chất lượng tài liệu chưa đủ tốt | Bộ kiểm thử repo cố định, so sánh trước/sau mỗi lần đổi prompt |
| Chi phí API vượt dự tính | Trần token mỗi repo, giới hạn chi tiêu trong Console, cache |
| Đối thủ lớn (công cụ tài liệu AI khác) | Tập trung ngách: dự án bàn giao, team offshore, tài liệu song ngữ về sau |
| Khách ngại đưa code lên dịch vụ ngoài | MVP chỉ repo public; về sau cân nhắc bản tự host |

## 9. Việc cần làm ngay tuần này
- [ ] Đọc hợp đồng lao động
- [ ] Mua tên miền (ví dụ `owlmap.dev` hoặc `owlmap.io.vn` — tra xem còn trống)
- [ ] Tạo email theo tên miền
- [ ] Đẩy repo này lên GitHub, bật GitHub Pages cho thư mục `landing/`
- [ ] Kết nối form đăng ký chờ (xem `README.md`)

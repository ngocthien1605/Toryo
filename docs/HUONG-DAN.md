# Hướng dẫn sử dụng Thien Orchestrator (bản prototype M0)

> Dành cho: anh Thiện — D365FO technical consultant, dùng PowerShell 7, Claude Code và Codex.
> Thuật ngữ kỹ thuật giữ nguyên tiếng Anh.

---

## 1. Thien là gì? — ví dụ so sánh với dự án D365FO

Hãy hình dung một dự án D365FO quen thuộc:

| Dự án D365FO | Trong Thien |
|---|---|
| **FDD** (Functional Design Document) do BA/consultant viết | **`TASK.md`** — anh viết: mục tiêu + `AC-1`, `AC-2`… (acceptance criteria) |
| **TDD** (Technical Design) do Tech Lead viết | **PLAN** — Claude viết (chỉ đọc code, không sửa được gì) |
| Khách hàng / PM **duyệt TDD** trước khi code | **Plan gate** — Thien dừng lại hỏi anh `y / n / q` |
| **Developer** code trên máy dev riêng, branch riêng | **Codex** code trong **git worktree** riêng, sandbox chỉ ghi được trong worktree đó |
| Developer **check-in** changeset | **Thien** tự tạo **checkpoint commit** (agent không được commit) |
| **Build pipeline + unit test** (Azure DevOps) | **Verify** — Thien chạy `scripts/build.ps1`, `scripts/test.ps1` (hoặc `commands.verify`) |
| **Code review** của Tech Lead, tick từng requirement | **Review** — Claude chấm *đúng từng* `AC-n`, kèm evidence, liệt kê issues |
| Review fail → dev sửa → review lại | **Fix cycle** — tối đa 2 vòng (mặc định), feedback được chuyển nguyên văn cho Codex |
| **Merge PR** vào main — người có quyền quyết định | **Anh** merge. Thien **không bao giờ** merge hay push |

Tóm lại: **Thien là "quản đốc công trường"**. Nó không tự viết code; nó *điều phối* hai "nhân viên" AI,
giữ cho mỗi người chỉ làm đúng phần việc của mình, ghi biên bản (log, diff, kết quả test) cho mọi bước,
và chỉ dừng lại hỏi anh ở những điểm cần con người quyết định.

Vì sao dùng 2 agent khác nhau? Giống như **developer không tự review code của mình**: Claude (planner/reviewer)
và Codex (implementer) là hai "người" độc lập, nên review khách quan hơn.

---

## 2. Flow chi tiết của `thien spike pipeline`

```text
 anh viết TASK.md (AC-1..n)
        │
        ▼
 ┌─────────────── SETUP (Thien) ────────────────────────────────────────────┐
 │ • đọc AC ids (bắt buộc ≥1, không trùng)                                   │
 │ • kiểm tra Git filters / submodules (an toàn)                             │
 │ • git worktree add  <worktrees_root>\<ID>  trên branch  thien/<ID>        │
 │ • tạo run store: %LOCALAPPDATA%\thien\runs\<ID>-<time>\runstore\          │
 └──────────────────────────────────────────────────────────────────────────┘
        │
        ▼
 1. PLAN — Claude (read-only: Read/Grep/Glob, --restricted, JSON schema)
        │  → plan.json + PLAN.md
        │  • plan muốn đụng file protected? → tự động bắt làm lại
        ▼
 2. PLAN GATE — anh:  y = duyệt · n = từ chối + lý do (Claude sửa plan, tối đa 3 lần) · q = dừng
        │
        ▼
 ┌─► 3. IMPLEMENT / FIX — Codex (sandbox workspace-write, chỉ trong worktree)
 │      │
 │      ▼
 │   4. GUARDS — Thien
 │      • HEAD guard: agent tự commit? → soft reset
 │      • protected paths (AGENTS.md, scripts verify, TASK.md, .claude/, .codex/…)
 │        bị sửa? → khôi phục, bắt Codex làm lại
 │      ▼
 │   5. CHECKPOINT — Thien commit:  "thien(<ID>): implement"  + trailers
 │      → DIFF.patch (base→HEAD), DIFF-incremental.patch (từ lần review trước)
 │      ▼
 │   6. VERIFY — build → test (dừng ở lệnh fail đầu tiên)
 │      • fail → quay lại 3 với log lỗi (KHÔNG sang review)
 │      ▼
 │   7. REVIEW — Claude (read-only) → review.json + REVIEW.md
 │      • Thien kiểm tra: đúng tập AC ids, có evidence thật, decision hợp lệ
 │        (sai → bắt Claude sửa, tối đa 2 lần)
 │      • "approved" nhưng còn AC chưa đạt / issue blocking → coi như changes_requested
 │      ▼
 │   approved ──────────────► DONE: in lệnh merge cho anh
 │   blocked  ──────────────► STOP (cần người)
 └── changes_requested ───── (nếu còn vòng fix) · hết vòng → ESCALATED
```

**Nguyên tắc an toàn quan trọng** (đã được kiểm chứng bằng M0 trên máy anh):

- Claude chạy `--restricted --safe-mode`, chỉ có tool Read/Grep/Glob, không MCP, không hooks → **không sửa được file**.
- Codex chạy `--ignore-user-config` + sandbox `workspace-write` (Windows `unelevated`) → chỉ ghi trong worktree.
- Git do Thien gọi luôn ở chế độ "hardened" (tắt hooks, fsmonitor, external diff, textconv, filter lạ).
- Mọi process agent chạy trong **Windows Job Object** → Thien bị kill thì cả cây process cũng chết theo.
- Branch `main` của anh **không bị đụng tới** — mọi thứ nằm ở branch `thien/<ID>`.

---

## 3. Cài đặt

Anh đã hoàn tất phần lớn khi chạy `m0-smoke.ps1`. Checklist:

| Thành phần | Kiểm tra | Ghi chú |
|---|---|---|
| PowerShell 7 | `pwsh -v` | |
| Git for Windows | `git --version` | đã cài |
| Claude Code (đã login) | `claude --version` | 2.1.283 trên máy anh |
| Codex CLI (đã login ChatGPT) | `codex login status` | 0.157.1 |
| thien.exe | `C:\.thien\AgentOrchestrator\thien\bin\thien.exe --version` | prebuilt, không cần cài Rust |
| thien.yaml | `C:\.thien\m0\thien.yaml` | do `m0-smoke.ps1` sinh ra |

Thiết lập cho phiên PowerShell (hoặc thêm vào `$PROFILE` để dùng lâu dài):

```powershell
Set-Alias thien C:\.thien\AgentOrchestrator\thien\bin\thien.exe
$env:THIEN_CONFIG = 'C:\.thien\m0\thien.yaml'      # để khỏi gõ --config mỗi lần
thien doctor                                        # kiểm tra tổng quát
```

> Nếu sau này thay đổi đường dẫn claude/codex, chạy lại
> `pwsh -NoProfile -ExecutionPolicy Bypass -File C:\.thien\AgentOrchestrator\thien\scripts\m0-smoke.ps1 -SkipSmoke`
> để sinh lại `thien.yaml`.

---

## 4. Cấu hình

### 4.1 `thien.yaml` (config của repo / dự án)

```yaml
version: 1
project:
  root: C:\.thien\m0\calc                 # repo mặc định (pipeline dùng --repo nên ít quan trọng)
  worktrees_root: C:\.thien\m0\worktrees  # nơi tạo worktree cho từng task
profiles:                                 # "tài khoản" để chạy agent
  claude-max:
    kind: claude
    billing: subscription                 # dùng gói Claude Max (không API key)
    exe: C:\Users\nguye\.local\bin\claude.exe
    env: { CLAUDE_CODE_GIT_BASH_PATH: C:\Program Files\Git\bin\bash.exe }
  codex-openai:
    kind: codex
    billing: subscription                 # ChatGPT Plus login
    exe: C:\Users\nguye\AppData\Roaming\npm\codex.cmd
    codex:
      sandbox_mode: workspace-write
      windows_sandbox: unelevated
roles:                                    # role → profile
  planner:     { profile: claude-max }
  implementer: { profile: codex-openai, fallback: [claude-max] }   # hết quota → Claude làm thay
  reviewer:    { profile: claude-max }
# --- tuỳ chọn, thêm tay khi cần ---
commands:
  verify:                                 # nếu bỏ trống: tự dùng scripts/build.ps1 + scripts/test.ps1
    - { name: build, program: pwsh, args: [-NoProfile, -NonInteractive, -File, scripts/build.ps1], timeout_min: 10 }
    - { name: test,  program: pwsh, args: [-NoProfile, -NonInteractive, -File, scripts/test.ps1],  timeout_min: 20 }
policy:
  protected_paths: ["docs/**", "*.lock"]  # thêm các file agent không được sửa (glob)
```

Thêm model riêng cho role: `planner: { profile: claude-max, model: opus }` (hoặc dùng `--claude-model` / `--codex-model` trên dòng lệnh).

Muốn dùng Codex qua **Foundry (API)**: thêm `[profiles.foundry]` trong `~/.codex/config.toml`, chạy lại
`m0-smoke.ps1 -CodexProfiles openai,foundry -SkipSmoke`, rồi dùng `--implementer codex-foundry`.

### 4.2 Machine config (tuỳ chọn)

`%USERPROFILE%\.thien\thien.local.yaml` — ghi đè `profiles`/`roles` theo máy, và khai báo
`git.trusted_filters` (ví dụ Git LFS). Không có file này cũng không sao.

### 4.3 Dữ liệu được lưu ở đâu?

`%LOCALAPPDATA%\thien\` (hoặc `$env:THIEN_DATA_DIR` nếu anh đặt):

```text
runs\BR-001-20260927T130000Z\
  runstore\TASK.md  PLAN.md  plan.json  DIFF.patch  DIFF-incremental.patch
           VERIFY_RESULT.json  REVIEW.md  review.json  SUMMARY.md
  plan-1.prompt.md / .stdout.jsonl / .stderr.txt / .outcome.json    ← mỗi lượt agent
  implement.* , review-0-0.* , fix-1.* ...
m0\results.jsonl                                                    ← lịch sử mọi lần chạy
```

Mỗi lượt agent được lưu đầy đủ prompt + stream JSON (đã redact secret) → dễ debug "vì sao AI làm vậy".

---

## 5. Các lệnh chính

| Lệnh | Dùng khi |
|---|---|
| `thien doctor` | kiểm tra exe, profile, config |
| `thien spike pipeline --repo <repo> --task <TASK.md>` | **chạy 1 task hoàn chỉnh** (lệnh chính) |
| `thien spike init-fixture --app batch-rename --into <dir>` | tạo repo mẫu để tập |
| `thien spike smoke` | test nhanh cả 2 agent trên fixture calculator (không có gate) |
| `thien spike report --out <file.md>` | tổng hợp kết quả M0 |
| `thien spike render --kind plan <plan.json>` | xem lại JSON dưới dạng Markdown |

Tuỳ chọn của `pipeline`:

| Option | Mặc định | Ý nghĩa |
|---|---|---|
| `--repo` | (bắt buộc) | repo Git; branch hiện tại là base |
| `--task` | (bắt buộc) | file task; đường dẫn tương đối theo thư mục hiện tại hoặc theo repo |
| `--id` | heading `# BR-001 …` hoặc tên file | tên task → tên worktree + branch `thien/<id>` |
| `--base` | branch hiện tại | base branch/commit |
| `--claude-profile` | `roles.planner` | profile Claude cho plan + review |
| `--implementer` | `roles.implementer` | profile code chính (tên cũ `--codex-profile` vẫn dùng được) |
| `--implementer-fallback` | `roles.implementer.fallback`, nếu không có thì profile Claude | danh sách dự phòng khi hết quota, ví dụ `codex-foundry,claude-max` |
| `--no-fallback` | tắt | hết quota thì dừng, không chuyển agent |
| `--simulate-quota` | tắt | giả lập agent chính hết quota để thử đường fallback |
| `--claude-model` / `--codex-model` | mặc định của CLI | chọn model |
| `--max-fix-cycles` | 2 | số vòng sửa tối đa |
| `--mode` | `restricted` | launch mode của Claude (M0 đã xác nhận `restricted` chạy được) |
| `--yes` | tắt | bỏ qua plan gate (chỉ dùng cho demo) |

Exit code: `0` = approved, khác `0` = dừng (xem `SUMMARY.md`).

### 5.1 Fallback khi Implementer hết quota

Giống **ca trực dự phòng**: Codex nghỉ vì hết quota thì người tiếp theo trong danh sách vào làm tiếp, không bỏ dở task.

```text
Codex (codex-openai) ──hết quota──► Claude (claude-max) làm tiếp
        │ 429 / overloaded tạm thời → thử lại 2 lần (chờ 30 s, 60 s) rồi mới chuyển
        └ lỗi khác (auth, bad request…) → KHÔNG chuyển, dừng như bình thường
```

- **Nhận biết**: Codex báo `usage limit` / `quota` / `insufficient_quota`; Claude báo `usage limit reached`,
  `credit balance`, `billing_error` → lớp lỗi `QuotaExceeded`.
- **Chuyển một lần, giữ luôn**: các vòng fix sau dùng thẳng agent dự phòng, không gọi lại Codex.
- **Code dở được giữ lại**: Thien liệt kê các file đang thay đổi dở (chưa commit) trong mục *Hand-over* của prompt,
  agent dự phòng kiểm tra, giữ phần đúng và làm tiếp.
- **Claude làm Implementer** bị giới hạn chặt: chỉ `Read/Grep/Glob/Edit/Write`, **không có Bash/PowerShell**,
  `--restricted` + `--permission-mode acceptEdits` → chỉ sửa được file trong worktree; file settings/git/cấu hình tool
  bị từ chối. TASK.md và PLAN.md được đưa thẳng vào prompt (không mở quyền vào run store). Build/test vẫn do Thien chạy.
- **Minh bạch**: commit checkpoint có trailer `Thien-Implementer: <profile>`; `SUMMARY.md` có mục *Implementer fallback*.
- **Lưu ý**: khi Claude vừa code vừa review, review chạy ở session mới nhưng kém độc lập hơn cặp Codex/Claude
  (Thien in cảnh báo). Claude Max dùng chung quota cho plan, review và code.

Thử đường fallback mà không cần chờ hết quota thật:

```powershell
thien spike pipeline --repo C:\.thien\demo\batch-rename --task tasks\BR-001.md --id BR-001-fb --simulate-quota
```

---

## 6. Thực hành: dự án mẫu **BatchRename** (PowerShell module)

Module đổi tên file hàng loạt. Trạng thái ban đầu chỉ có 1 hàm `ConvertTo-SafeFileName`.
Có sẵn 2 task: **BR-001** (đổi tên theo regex, hỗ trợ `-WhatIf`, chống trùng tên) và **BR-002** (đánh số thứ tự `{n}`).

### Bước 1 — Tạo repo mẫu

```powershell
Set-Alias thien C:\.thien\AgentOrchestrator\thien\bin\thien.exe
$env:THIEN_CONFIG = 'C:\.thien\m0\thien.yaml'

thien spike init-fixture --app batch-rename --into C:\.thien\demo\batch-rename
cd C:\.thien\demo\batch-rename
git log --oneline                     # 1 commit trên main
pwsh -NoProfile -File scripts\test.ps1   # 3 test pass
code .                                # (tuỳ chọn) xem AGENTS.md, tasks\BR-001.md
```

Để ý 3 file quan trọng — đây cũng là "khuôn" cho dự án thật:

- **`AGENTS.md`** — quy tắc cho AI (coding convention, không dùng Pester, không commit…). Codex đọc tự động.
- **`scripts/build.ps1`, `scripts/test.ps1`** — "build pipeline". Thien dựa vào exit code của chúng.
- **`tasks/BR-001.md`** — FDD thu nhỏ với `AC-1..AC-5`.

### Bước 2 — Chạy BR-001

```powershell
thien spike pipeline --repo C:\.thien\demo\batch-rename --task tasks\BR-001.md
```

Anh sẽ thấy:

1. `── PLAN (Claude) ──` → khoảng 30–60 giây, in ra PLAN (Understanding, Files, Steps, Risks…).
2. Câu hỏi: `▶ Approve this plan for BR-001? [y]es / [n]o / [q]uit:`
   - **Thử `n`** lần đầu để trải nghiệm, ví dụ lý do: `Thêm test cho trường hợp file đích đã tồn tại sẵn`.
     Claude sẽ sửa plan (resume đúng session cũ) và hỏi lại.
   - Gõ `y` để duyệt.
3. `── IMPLEMENT (Codex) ──` → 1–3 phút. Codex viết `Rename-ByPattern` + `tests\RenameByPattern.Tests.ps1`.
4. `✔ checkpoint …` → `── VERIFY ──` build/test.
5. `── REVIEW (Claude) ──` → bảng AC-1..AC-5 (Met / Evidence) + Issues.
6. Nếu `changes_requested` hoặc test fail → `── FIX-1 (Codex) ──` … tự động.
7. Kết thúc: `BR-001: APPROVED ✔` cùng các lệnh gợi ý.

### Bước 3 — Kiểm tra kết quả (như review PR)

```powershell
cd C:\.thien\demo\batch-rename
git log --oneline --format="%h %s" main..thien/BR-001     # các checkpoint của Thien
git diff main...thien/BR-001                              # toàn bộ thay đổi
git log -1 --format=%B thien/BR-001                       # xem trailers Thien-Task / Thien-Stage
explorer "$env:LOCALAPPDATA\thien\runs"                   # PLAN.md, REVIEW.md, SUMMARY.md…

# Tự tay thử module ở worktree:
cd C:\.thien\m0\worktrees\BR-001
Import-Module .\src\BatchRename.psm1 -Force
New-Item -ItemType Directory $env:TEMP\br-demo -Force | Out-Null
'a','b' | % { New-Item "$env:TEMP\br-demo\IMG_$_.jpg" -Force | Out-Null }
Rename-ByPattern -Path $env:TEMP\br-demo -Pattern '^IMG_' -Replacement 'Holiday_' -WhatIf
```

### Bước 4 — Merge (quyết định của anh)

```powershell
cd C:\.thien\demo\batch-rename
git merge --squash thien/BR-001
git commit -m "BR-001: Rename-ByPattern"
git worktree remove --force C:\.thien\m0\worktrees\BR-001
git branch -D thien/BR-001
```

### Bước 5 — BR-002 (task nối tiếp, dựa trên code BR-001 đã merge)

```powershell
thien spike pipeline --repo C:\.thien\demo\batch-rename --task tasks\BR-002.md
```

Lần này thử **tự viết thêm BR-003** (ví dụ: `-Recurse`, hoặc xuất log CSV các file đã đổi tên) theo mẫu ở mục 7.

---

## 7. Áp dụng cho dự án thật

### 7.1 Chuẩn bị repo (làm 1 lần)

1. Repo Git có commit đầu tiên, working tree sạch.
2. `AGENTS.md` ở gốc repo: convention, cấu trúc thư mục, lệnh build/test, những điều cấm.
   (Codex nạp tự động; Claude đọc khi lập plan/review vì nó được phép Read toàn bộ worktree.)
3. Script verify **không tương tác, trả exit code ≠ 0 khi lỗi** (`scripts/build.ps1`, `scripts/test.ps1`),
   hoặc khai báo `commands.verify` trong `thien.yaml`.
4. Các file không cho AI sửa → `policy.protected_paths`.

### 7.2 Mẫu TASK.md

```markdown
# ABC-123 — <tiêu đề ngắn>

## Goal
<1–3 câu: vì sao cần, ai dùng>

## Acceptance criteria
- AC-1: <hành vi quan sát/kiểm tra được — input → output cụ thể>
- AC-2: <trường hợp lỗi: thông điệp chứa '...' >
- AC-3: Tests in `tests/Xyz.Tests.ps1` cover AC-1..AC-2.

## Scope
- <file/thư mục được phép sửa>

## Out of scope
- <những gì KHÔNG làm>

## Verification
- `pwsh -NoProfile -File scripts/test.ps1`
```

Mẹo viết AC (giống viết FDD tốt):

- Mỗi AC **kiểm chứng được** (có tên hàm, tham số, thông điệp lỗi, tên file test). Tránh "code sạch", "nhanh".
- Có ít nhất một AC về **test** → Codex buộc phải viết test, Claude có evidence để chấm.
- Task nhỏ (≤ ½ ngày công người) → plan ngắn, review chính xác, ít vòng fix.

### 7.3 Với D365FO?

X++ cần build trên VM dev (xppc / MSBuild, DB sync) — nặng và lâu. Hiện tại nên dùng Thien cho các phần
chạy được bằng script trên máy anh: **PowerShell tooling (deploy, LCS/Azure DevOps automation), Power Platform
(solution unpack/pack + test), Azure Functions / Logic Apps code, data migration scripts (DMF packages)**.
Khi có lệnh build/test X++ chạy không tương tác trên dev VM, chỉ cần khai báo nó trong `commands.verify`.

---

## 8. Giới hạn của bản prototype (cần biết)

| Giới hạn | Hệ quả / cách xử lý |
|---|---|
| Verify chạy **với quyền user của anh** trong worktree (chưa có Low-IL boundary) | Chỉ dùng cho repo và task anh tin tưởng. Script test do AI viết sẽ được chạy thật! |
| Chưa có resume sau crash / Ctrl+C | Chạy lại với `--id` khác, hoặc xoá worktree + branch cũ (lệnh in ra cuối mỗi lần chạy) |
| Fallback mới áp dụng cho Implementer (planner/reviewer Claude hết quota thì dừng) | Chờ reset quota rồi chạy lại |
| Chưa có budget cap theo token | Xem token ở cuối mỗi lượt và trong `SUMMARY.md`; giữ `--max-fix-cycles` nhỏ |
| Một task mỗi lần chạy, chạy tuần tự | Chạy nhiều terminal với các `--id` khác nhau nếu cần |
| Protected paths dùng glob đơn giản | Ghi rõ quy tắc trong `AGENTS.md` để agent tự tránh |

---

## 9. Xử lý sự cố

| Triệu chứng | Cách xử lý |
|---|---|
| `worktree … already exists` / `branch thien/<ID> already exists` | Dùng `--id BR-001b`, hoặc chạy 2 lệnh dọn dẹp in ở cuối lần trước |
| `TASK.md has no AC-<n>: items` | Cần heading `## Acceptance criteria` và dòng `- AC-1: …` |
| `untrusted_git_filter` | Repo dùng Git LFS/filter → khai báo `git.trusted_filters` trong machine config |
| Plan lỗi `ProcessCrashed` / không có JSON | `claude` chưa login hoặc hết quota → chạy `claude` một lần để kiểm tra; xem `plan-1.stderr.txt` |
| Codex báo hết quota / rate limit | Thien tự chuyển sang agent dự phòng (mục 5.1). Nếu tất cả đều hết: đợi reset, hoặc thêm `--implementer-fallback codex-foundry,claude-max` |
| Verify fail mãi | Mở `VERIFY_RESULT.json`; tự chạy `pwsh -File scripts\test.ps1` trong worktree |
| `doctor` báo `cannot give … a Low integrity label` (verify root) | Thư mục tạo trực tiếp dưới `C:\` chỉ cho user quyền Modify (thiếu `WRITE_OWNER`). Để `verify.verify_root` trong `%LOCALAPPDATA%` (mặc định), xem F7 trong `M0-FINDINGS.md` |
| Test tạo thư mục tạm bị lỗi quyền khi Codex chạy | Thien đã cấp TEMP riêng `<worktree>\.thien-tmp` (git bỏ qua). Nếu vẫn lỗi, xem `implement.meta.json` có biến `TEMP` không |
| Muốn xem AI "nghĩ" gì | `runs\<ID>-…\*.stdout.jsonl` (stream đầy đủ) và `*.prompt.md` (prompt đã gửi) |

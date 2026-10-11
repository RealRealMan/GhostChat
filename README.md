# GhostChat

Private, end-to-end encrypted, live 1-to-1 chat for family, with a movie-style hacker terminal look.

**[English](#english) · [繁體中文](#繁體中文) · [For developers](#for-developers)**

---

# English

## 1. What GhostChat is

- **End-to-end encrypted.** Only you and the person you talk to can read messages and files.
- **Live only.** Both people must have GhostChat open. Nothing is queued or stored.
- **No chat history.** Messages live in memory only. Close the app and they are gone.
- **No servers to pay for.** Computers connect directly when possible, otherwise through a free relay that only sees encrypted data.
- **Family only.** Nobody can reach you unless you paired with them first.

What it does **not** do: it does not hide *that* two computers talked (internet providers and the relay can see IP addresses and timing), and it cannot protect a computer that already has malware.

## 2. Encryption and why it is secure

GhostChat does **not** invent its own cryptography. It uses well-known, widely used open-source libraries: [iroh](https://www.iroh.computer) 1.3 for networking, rustls / *ring* for TLS, and RustCrypto for local encryption.

| Layer | Technology | What it does for you |
|---|---|---|
| **Your identity** | Ed25519 key pair, created on your computer | Your public key *is* your identity (not your codename, not your IP). The secret key is kept in Windows Credential Manager / macOS Keychain / Linux Secret Service and never leaves your computer. |
| **The connection** | QUIC + **TLS 1.3**, both sides authenticated with their Ed25519 keys | Every connection proves both computers hold the right secret keys. No passwords, no certificate authority, no server in the middle that can pretend to be someone. |
| **Key exchange** | Ephemeral **X25519** (Diffie-Hellman), new for every connection | **Forward secrecy:** even if someone records your traffic and later steals your identity key, they cannot decrypt past conversations. |
| **Message and file data** | **AES-256-GCM** (or ChaCha20-Poly1305), authenticated encryption | Content is unreadable to ISPs, Wi-Fi owners and the relay. Any changed, injected or replayed packet is rejected. |
| **Who may connect** | Allowlist checked right after the TLS handshake | Strangers are cut off before any data is exchanged. Pair requests are only accepted during your `/invite` window and still need `/accept`. |
| **Stopping impersonation** | Safety words: BLAKE3 hash of both public keys → 2 × 6 BIP39 words (66 bits each way) + randomart picture | If someone swapped a key in transit, the words and picture would not match on your call. |
| **Files** | BLAKE3 checksum of the whole file | A corrupted or incomplete file is detected and deleted. Nothing is received without your `/accept`. |
| **Contact list on disk** | XChaCha20-Poly1305, key in the OS secure store (Argon2id passphrase if no secure store exists) | Someone copying your files cannot read who your contacts are. |
| **No traces** | Messages only in memory, wiped (zeroized) on `/clear`, exit and panic; no log files; own window (no terminal scrollback); crash dumps suppressed where possible | Closing GhostChat leaves no conversation on disk. |

**Why the relay cannot read anything:** encryption happens on your computer and decryption only on the other person's computer. The relay only forwards encrypted QUIC packets; it never has the keys.

**Honest limits** (details in [`SECURITY.md`](SECURITY.md)):

- Metadata is visible: the relay and internet providers see IP addresses, timing and data volume. `relay_only` hides your IP from contacts, not from the relay.
- Forward secrecy is per connection, not per message (no Signal-style ratchet).
- No post-quantum protection yet.
- A computer with malware, or someone at your keyboard, can see everything.
- Received files are normal files on your disk. Windows/macOS may also keep fragments of memory in swap or hibernation files.

## 3. Install

You get one installer file from the person who invites you. Only accept it through a trusted channel such as Signal.

The app is not code-signed (signing costs money every year), so your computer warns you the first time. This is expected.

### Windows 10 / 11

1. Double-click `ghostchat_<version>_x64-setup.exe`.
2. A blue box says **"Windows protected your PC"**. Click the small link **More info**.
3. Click **Run anyway**.
4. Click **Next → Install → Finish**. No administrator password is needed.

A green ghost **GhostChat** icon appears on the desktop and in the Start menu.

**Uninstall:** Settings → Apps → GhostChat → Uninstall. Leave **"Also delete my GhostChat identity and contacts"** unticked if you may reinstall.

### macOS (not yet tested on a real Mac)

1. Open `GhostChat.dmg` and drag **GhostChat** into **Applications**.
2. Open GhostChat. macOS blocks it: click **Done** (not "Move to Bin").
3. Go to **System Settings → Privacy & Security**, scroll down, click **Open Anyway**, enter your Mac password.
   On macOS 14 or older: right-click GhostChat → **Open → Open**.
4. If asked about the **Keychain**, click **Always Allow**.

### Linux (not yet tested)

`GhostChat.AppImage`: right-click → Properties → Permissions → allow executing, then double-click. Or install the `.deb`.

## 4. First start

1. Open GhostChat. Press any key to skip the boot animation.
2. Type your **codename** (3–16 letters, digits, `-` or `_`, e.g. `NIGHTOWL`) and press **Enter**.

Your secret identity key is created and stored in your computer's secure key store. It never leaves your computer.

## 5. Pair with a family member

Only **one** person sends a key: the **inviter**. The other person is the **contact** (in this example the contact's codename is `NIGHTOWL`).

| Step | Who | What to do |
|---|---|---|
| 1 | Inviter | Type `/invite`. Your Contact Key appears and is copied. A 10-minute pairing window opens. |
| 2 | Inviter | Send the Contact Key to the contact (Signal, WhatsApp, email — it is not secret). |
| 3 | Contact | Type `/add ` and paste the key, press Enter. |
| 4 | Inviter | `INCOMING PAIR REQUEST: NIGHTOWL` appears. Type `/accept`. |

You now see each other on the left, marked red `[?]` (unverified).

## 6. Verify (once, on a voice or video call)

Verification proves nobody swapped the key on the way. **Do it by voice or video, never by text.**

1. Call each other. Both type `/verify NAME` (e.g. `/verify NIGHTOWL`).
2. Check both screens show **the same picture**.
3. Each person **reads their own 6 words aloud**, and **types the 6 words the other reads**, then presses Enter.
4. When the words match, the contact turns `[V]` (verified). Your words stay on screen until you press Enter, so the other person can finish.

Each person verifies the other separately. If the other person missed your words, type `/verify NAME` again: it shows your words without asking you to type anything.

## 7. Everyday use

- **Chat:** click a name on the left (or `/chat NAME`), type, press Enter. GhostChat connects automatically when both of you are online.
- **Alerts:** when GhostChat is in the background (or showing the idle screen), a new message, pair request or file offer flashes the taskbar icon (Dock icon on macOS). No system pop-up is used, so nothing ends up in the OS notification history. Turn off with `/flash off`. On Windows this also works while GhostChat is minimized; on macOS and Linux only while the window is not minimized.
- **Send a file:** drag it onto the window, or `/send`. The other person must `/accept` it. Files arrive in `Downloads/GhostChat/`.
- **Commands start with `/`.** Text without `/` is sent as a message. To send a message that starts with `/`, type `//`.

**Status bar (top right)** always shows the real state:

| Shows | Meaning |
|---|---|
| `E2EE` | The channel is end-to-end encrypted |
| `DIRECT` / `RELAY` | Connected computer-to-computer / through the relay (still encrypted) |
| `VERIFIED` / `UNVERIFIED` | Whether you verified this contact |
| `NET ONLINE` | GhostChat is reachable on the internet |
| `PAIRING OPEN 9:59` | Your `/invite` window is open |

**Contact list symbols:** `●` online · `○` offline · `~` connecting · `[V]` verified · `[?]` unverified · `[!]` key changed (blocked).
**Message marks:** `>` sent · `>>` delivered · `x` failed.

**Version** is shown at the bottom of the contact list.

## 8. Commands

### Pairing and contacts

| Command | Example | What it does |
|---|---|---|
| `/invite` | `/invite` | Show your Contact Key (copied to clipboard) and open a 10-minute pairing window. |
| `/invite stop` | `/invite stop` | Close the pairing window early. |
| `/add KEY` | `/add GC1-...` | Send a pair request using someone's Contact Key. `/add` alone opens a `.ghostkey` file. |
| `/accept` | `/accept` | Accept a pair request or an incoming file. |
| `/reject` | `/reject` | Refuse a pair request or an incoming file. |
| `/verify NAME` | `/verify NIGHTOWL` | Verify a contact with safety words. On a verified contact, shows your words again. |
| `/contacts` | `/contacts` | List contacts and whether they are verified. |
| `/alias NAME NEW` | `/alias NIGHTOWL CONTACT-1` | Show a contact under your own name for them. `/alias NAME` resets it. |
| `/remove NAME` | `/remove NIGHTOWL` | Delete a contact (asks `/yes` or `/no`). You must pair again afterwards. |

### Chatting

| Command | Example | What it does |
|---|---|---|
| `/chat NAME` | `/chat NIGHTOWL` | Open a conversation (same as clicking the name). `/chat` alone goes to the console. |
| `/connect [NAME]` | `/connect NIGHTOWL` | Connect now instead of waiting for the automatic retry. |
| `/disconnect [NAME]` | `/disconnect` | Close the secure channel. |
| `/send [FILE]` | `/send` | Send a file. Without a file name, a file picker opens. |
| `/clear` | `/clear` | Wipe the open conversation from screen and memory. |

### You and the look

| Command | Example | What it does |
|---|---|---|
| `/codename NAME` | `/codename GHOST-01` | Change your codename. Contacts see the new name. |
| `/mykey` | `/mykey` | Show your Contact Key and picture. `/mykey save` saves it as a `.ghostkey` file. |
| `/theme NAME` | `/theme amber` | Colours: `green`, `amber` or `ice`. |
| `/help` | `/help` | Show all commands. |

### Safety

| Command | Example | What it does |
|---|---|---|
| `/panic` or **F12** | F12 | Close all connections, wipe everything in memory, exit immediately. |
| `/yes`, `/no` | `/yes` | Answer a confirmation question. |
| `/quit` | `/quit` | Close GhostChat normally. |

## 9. Troubleshooting

| Problem | What to do |
|---|---|
| A command was sent as a message | You forgot the `/` at the start. |
| Contact stays offline `○` | They must have GhostChat open. It retries every 30 seconds; or type `/connect`. |
| Red **INTRUSION WARNING: KEY MISMATCH** | Someone with a new key uses a known codename. Chat is blocked. Only continue if that person told you **by voice** that they reinstalled, then verify again. |
| Pair request never arrives | The inviter's 10-minute window may have closed. Ask them to `/invite` again. |

## 10. Settings

File `settings.json` in:
Windows `%APPDATA%\GhostChat\data\` · macOS `~/Library/Application Support/GhostChat/` · Linux `~/.local/share/ghostchat/`

| Key | Default | Meaning |
|---|---|---|
| `relay_only` | `false` | Never connect directly, so contacts never see your IP address |
| `flash` | `true` | Same as `/flash` |
| `auto_connect` | `true` | Retry contacts every 30 seconds |
| `idle_minutes` | `5` | Show the Matrix screen after this many idle minutes (0 = off) |
| `pairing_minutes` | `10` | Length of the `/invite` window |
| `max_file_mb` | `2048` | Larger incoming files are refused |
| `downloads_dir` | `null` | Where files are saved (default `Downloads/GhostChat`) |
| `panic_wipes_identity` | `false` | Same as `/panicwipe` |
| `relay_urls` | `[]` | Your own relay servers (empty = free public relays) |
| `pkarr_relay` | `null` | Your own discovery server (null = public default) |

---

# 繁體中文

## 1. GhostChat 是甚麼

- **端對端加密**：只有你和對方看得到訊息和檔案。
- **只限即時對話**：雙方都要開着 GhostChat，訊息不會排隊等候或被儲存。
- **不留對話記錄**：訊息只存在記憶體，關掉程式便消失。
- **不用付錢租伺服器**：電腦之間盡量直接連線，否則經免費中繼站轉送，中繼站只看到加密資料。
- **只限家人**：沒有和你配對過的人無法聯絡你。

GhostChat **不能**做到的事：無法隱藏「兩部電腦曾經連線」這件事（網絡供應商和中繼站看得到 IP 位址和時間），也無法保護已經中了惡意軟件的電腦。

## 2. 加密技術與安全原理

GhostChat **沒有**自創加密方法，而是採用廣泛使用的開源程式庫：網絡用 [iroh](https://www.iroh.computer) 1.3，TLS 用 rustls / *ring*，本機加密用 RustCrypto。

| 層面 | 技術 | 對你的保障 |
|---|---|---|
| **你的身份** | 在你電腦上產生的 Ed25519 金鑰對 | 你的公開金鑰就是你的身份（不是代號，也不是 IP）。秘密金鑰存放在 Windows 認證管理員／macOS 鑰匙圈／Linux Secret Service，永遠不會離開你的電腦。 |
| **連線** | QUIC + **TLS 1.3**，雙方以 Ed25519 金鑰互相認證 | 每次連線都證明雙方持有正確的秘密金鑰。不用密碼，不依賴憑證機構，中間沒有伺服器可以冒充任何人。 |
| **金鑰交換** | 每次連線都產生新的臨時 **X25519** 金鑰（Diffie-Hellman） | **前向保密**：即使有人錄下你的通訊，之後再偷走你的身份金鑰，也無法解密以前的對話。 |
| **訊息與檔案內容** | **AES-256-GCM**（或 ChaCha20-Poly1305）認證加密 | 網絡供應商、Wi-Fi 擁有者和中繼站都看不到內容。任何被修改、插入或重播的封包都會被拒絕。 |
| **誰可以連線** | TLS 握手後立即檢查白名單 | 陌生人在交換任何資料前已被切斷。配對請求只在你的 `/invite` 時段內接受，而且仍要你 `/accept`。 |
| **防止冒充** | 安全字詞：用 BLAKE3 計算雙方公開金鑰 → 2 × 6 個 BIP39 英文字（每方向 66 位元）+ 圖案 | 如果金鑰在傳送途中被調包，通話時字詞和圖案就不會吻合。 |
| **檔案** | 整個檔案的 BLAKE3 校驗碼 | 損壞或不完整的檔案會被偵測並刪除。未經你 `/accept` 不會接收任何檔案。 |
| **磁碟上的聯絡人清單** | XChaCha20-Poly1305，金鑰存放在系統安全金鑰庫（沒有金鑰庫時用 Argon2id 密碼保護） | 即使有人複製你的檔案，也看不到你的聯絡人是誰。 |
| **不留痕跡** | 訊息只在記憶體，在 `/clear`、關閉和緊急退出時清除（zeroize）；沒有記錄檔；使用獨立視窗（沒有終端機捲動記錄）；盡可能停用當機傾印 | 關閉 GhostChat 後，磁碟上不會留下對話內容。 |

**為甚麼中繼站看不到內容：** 加密在你的電腦進行，解密只在對方電腦進行。中繼站只轉送加密的 QUIC 封包，從來沒有金鑰。

**目前不足**（詳情見 [`SECURITY.md`](SECURITY.md)）：

- 元數據看得到：中繼站和網絡供應商看得到 IP 位址、時間和資料量。`relay_only` 只會向聯絡人隱藏你的 IP，不會向中繼站隱藏。
- 前向保密以每次連線為單位，不是每則訊息（沒有 Signal 式的 ratchet）。
- 暫時沒有抗量子加密。
- 如果電腦有惡意軟件，或有人坐在你的電腦前，所有內容都會被看到。
- 收到的檔案是磁碟上的普通檔案。Windows／macOS 亦可能在虛擬記憶體或休眠檔案中保留記憶體片段。

## 3. 安裝

邀請你的人會給你一個安裝檔，請只經可信的渠道（例如 Signal）接收。

程式沒有付費簽名，所以第一次安裝時電腦會彈出警告，這是正常的，不是病毒。

### Windows 10 / 11

1. 雙擊 `ghostchat_<版本>_x64-setup.exe`。
2. 出現藍色「**Windows 已保護您的電腦**」，按細字「**其他資訊**」。
3. 按「**仍要執行**」。
4. 按「**下一步 → 安裝 → 完成**」，不需要管理員密碼。

桌面和開始功能表會出現綠色鬼仔 **GhostChat** 圖示。

**解除安裝**：設定 → 應用程式 → GhostChat → 解除安裝。如果日後可能再安裝，不要剔選「**Also delete my GhostChat identity and contacts**」（同時刪除身份及聯絡人）。

### macOS（尚未在真正的 Mac 測試）

1. 打開 `GhostChat.dmg`，把 **GhostChat** 拖入「**應用程式**」。
2. 打開 GhostChat，macOS 會阻擋，按「**完成**」（不要按「移到垃圾桶」）。
3. 到「**系統設定 → 私隱與保安**」，向下捲，按「**仍要打開**」，輸入 Mac 密碼。
   macOS 14 或更舊：在 GhostChat 按右鍵 →「**打開 → 打開**」。
4. 如果問及「**鑰匙圈**」，按「**永遠允許**」。

### Linux（尚未測試）

`GhostChat.AppImage`：按右鍵 → 內容 → 權限 → 允許執行，然後雙擊。或安裝 `.deb`。

## 4. 第一次開啟

1. 打開 GhostChat，按任何鍵可略過開機動畫。
2. 輸入你的**代號**（3–16 個英文字母、數字、`-` 或 `_`，例如 `NIGHTOWL`），按 **Enter**。

你的秘密身份金鑰會建立並儲存在電腦的安全金鑰庫，永遠不會離開你的電腦。

## 5. 和家人配對

只需**一方**傳送金鑰。傳送金鑰的一方稱為**邀請方**，另一方是**通話對象**（例子中通話對象的代號是 `NIGHTOWL`）。

| 步驟 | 誰 | 做甚麼 |
|---|---|---|
| 1 | 邀請方 | 輸入 `/invite`，畫面顯示聯絡金鑰（已自動複製），並開啟 10 分鐘配對時段。 |
| 2 | 邀請方 | 把聯絡金鑰傳給通話對象（Signal、WhatsApp、電郵都可以，金鑰不是秘密）。 |
| 3 | 通話對象 | 輸入 `/add `，貼上金鑰，按 Enter。 |
| 4 | 邀請方 | 畫面出現 `INCOMING PAIR REQUEST: NIGHTOWL`，輸入 `/accept`。 |

雙方左邊會出現對方名字，標示紅色 `[?]`（未核實）。

## 6. 核實（只需一次，用語音或視像通話）

核實是確保金鑰在傳送途中沒有被人調包。**必須用語音或視像，絕對不要用文字。**

1. 互相打電話，雙方輸入 `/verify 名字`（例如 `/verify NIGHTOWL`）。
2. 確認兩個畫面顯示**相同的圖案**。
3. 每人**讀出自己的 6 個英文字**，然後**輸入對方讀給你聽的 6 個字**，按 Enter。
4. 字詞相符後，聯絡人變成 `[V]`（已核實）。你的 6 個字會繼續留在畫面，直至你按 Enter，讓對方有時間輸入。

雙方要各自核實對方。如果對方未記下你的字，再輸入 `/verify 名字`，畫面會再次顯示你的 6 個字，不用重新輸入。

## 7. 日常使用

- **傾偈**：按左邊的名字（或 `/chat 名字`），輸入文字，按 Enter。雙方都在線時會自動連線。
- **提示**：GhostChat 在背景（或顯示閒置畫面）時，收到新訊息、配對請求或檔案，會令工作列圖示閃動（macOS 是 Dock 圖示跳動）。不會使用系統彈出通知，所以系統通知記錄不會留下任何內容。可用 `/flash off` 關閉。Windows 上即使視窗縮到最小亦有效；macOS 和 Linux 只在視窗未縮到最小時有效。
- **傳送檔案**：把檔案拖入視窗，或輸入 `/send`。對方要 `/accept` 才會開始傳送。檔案存放在 `下載/GhostChat/`。
- **指令以 `/` 開頭。** 沒有 `/` 的文字會當作訊息傳給對方。如果訊息本身要以 `/` 開頭，請輸入 `//`。
- **中文輸入**：可以用倉頡等中文輸入法；輸入指令時記得切換回英文。

**狀態列（右上角）** 永遠顯示真實狀態：

| 顯示 | 意思 |
|---|---|
| `E2EE` | 已端對端加密 |
| `DIRECT` / `RELAY` | 電腦之間直接連線／經中繼站（仍然加密） |
| `VERIFIED` / `UNVERIFIED` | 你是否已核實這位聯絡人 |
| `NET ONLINE` | GhostChat 已連上互聯網 |
| `PAIRING OPEN 9:59` | 你的 `/invite` 配對時段仍然開啟 |

**聯絡人符號**：`●` 在線 · `○` 離線 · `~` 連線中 · `[V]` 已核實 · `[?]` 未核實 · `[!]` 金鑰已改變（已封鎖）
**訊息符號**：`>` 已傳送 · `>>` 已送達 · `x` 失敗

**版本號碼**顯示在聯絡人清單底部。

## 8. 指令一覽

### 配對與聯絡人

| 指令 | 例子 | 作用 |
|---|---|---|
| `/invite` | `/invite` | 顯示你的聯絡金鑰（已複製）並開啟 10 分鐘配對時段。 |
| `/invite stop` | `/invite stop` | 提早關閉配對時段。 |
| `/add 金鑰` | `/add GC1-...` | 用對方的聯絡金鑰發出配對請求。只輸入 `/add` 可開啟 `.ghostkey` 檔案。 |
| `/accept` | `/accept` | 接受配對請求或傳入的檔案。 |
| `/reject` | `/reject` | 拒絕配對請求或傳入的檔案。 |
| `/verify 名字` | `/verify NIGHTOWL` | 用安全字詞核實聯絡人。對已核實的聯絡人會再次顯示你的字詞。 |
| `/contacts` | `/contacts` | 列出聯絡人及核實狀態。 |
| `/alias 名字 新名` | `/alias NIGHTOWL CONTACT-1` | 用你自訂的名字顯示聯絡人。`/alias 名字` 可還原。 |
| `/remove 名字` | `/remove NIGHTOWL` | 刪除聯絡人（會問 `/yes` 或 `/no`），之後要重新配對。 |

### 傾偈

| 指令 | 例子 | 作用 |
|---|---|---|
| `/chat 名字` | `/chat NIGHTOWL` | 打開對話（等於按左邊的名字）。只輸入 `/chat` 回到主控台。 |
| `/connect [名字]` | `/connect NIGHTOWL` | 立即連線，不用等自動重試。 |
| `/disconnect [名字]` | `/disconnect` | 關閉加密通道。 |
| `/send [檔案]` | `/send` | 傳送檔案。沒有指定檔案時會打開選擇視窗。 |
| `/clear` | `/clear` | 從畫面和記憶體清除目前對話。 |

### 個人與外觀

| 指令 | 例子 | 作用 |
|---|---|---|
| `/codename 名字` | `/codename GHOST-01` | 更改你的代號，聯絡人會看到新名字。 |
| `/mykey` | `/mykey` | 顯示你的聯絡金鑰和圖案。`/mykey save` 另存為 `.ghostkey` 檔案。 |
| `/theme 名稱` | `/theme amber` | 顏色主題：`green`、`amber` 或 `ice`。 |
| `/help` | `/help` | 顯示所有指令。 |

### 安全

| 指令 | 例子 | 作用 |
|---|---|---|
| `/panic` 或 **F12** | F12 | 關閉所有連線、清除記憶體內所有資料，立即退出。 |
| `/yes`、`/no` | `/yes` | 回答確認問題。 |
| `/quit` | `/quit` | 正常關閉 GhostChat。 |

## 9. 疑難排解

| 問題 | 解決方法 |
|---|---|
| 指令被當作訊息傳了出去 | 開頭漏了 `/`。 |
| 聯絡人一直離線 `○` | 對方要開着 GhostChat。程式每 30 秒自動重試，也可輸入 `/connect`。 |
| 紅色 **INTRUSION WARNING: KEY MISMATCH** | 有人用新金鑰冒用已知代號，對話已封鎖。只有對方**親口**告訴你他重新安裝過，才可繼續並重新核實。 |
| 收不到配對請求 | 邀請方的 10 分鐘時段可能已過，請對方再輸入 `/invite`。 |

## 10. 設定

設定檔 `settings.json` 位於：
Windows `%APPDATA%\GhostChat\data\` · macOS `~/Library/Application Support/GhostChat/` · Linux `~/.local/share/ghostchat/`

| 設定 | 預設 | 意思 |
|---|---|---|
| `relay_only` | `false` | 永不直接連線，聯絡人看不到你的 IP 位址 |
| `flash` | `true` | 等同 `/flash` |
| `auto_connect` | `true` | 每 30 秒自動重試連線 |
| `idle_minutes` | `5` | 閒置多少分鐘後顯示 Matrix 畫面（0 = 關閉） |
| `pairing_minutes` | `10` | `/invite` 配對時段長度 |
| `max_file_mb` | `2048` | 超過此大小的檔案會被拒絕 |
| `downloads_dir` | `null` | 檔案存放位置（預設 `下載/GhostChat`） |
| `panic_wipes_identity` | `false` | 等同 `/panicwipe` |
| `relay_urls` | `[]` | 自設中繼站（空白 = 免費公共中繼站） |
| `pkarr_relay` | `null` | 自設探索伺服器（null = 公共預設） |

---

# For developers

Spec: [`PRD-v0.3.md`](PRD-v0.3.md) · Design: [`ARCHITECTURE.md`](ARCHITECTURE.md) · Security: [`SECURITY.md`](SECURITY.md) · Status: [`PROGRESS.md`](PROGRESS.md) · Open decisions: [`OPEN-QUESTIONS.md`](OPEN-QUESTIONS.md)

Requires Rust 1.95+ (tested with 1.98.1). Linux also needs `libgtk-3-dev libxdo-dev libxkbcommon-dev libwayland-dev libdbus-1-dev`.

Run:

```bash
cargo run --release
```

Two copies on one computer (separate identities):

```bash
cargo run --release -- --profile alice
```

```bash
cargo run --release -- --profile bob
```

Flags: `--wipe-identity` (delete this computer's identity and contacts), `--help`. Debug logging to stderr only: set `GHOSTCHAT_DEBUG=1`.

Checks:

```bash
cargo fmt --check
```

```bash
cargo clippy --all-targets -- -D warnings
```

```bash
cargo test
```

Ignored tests that need the internet or the real OS key store:

```bash
cargo test --test internet -- --ignored --nocapture
```

```bash
cargo test --test ui real_stack -- --ignored --nocapture
```

Headless UI tests save screenshots of every screen to `target/ui-shots/`.

Windows installer (output in `dist/`):

```bash
cargo install cargo-packager --version 0.11.8 --locked
```

```bash
cargo packager --release --formats nsis
```

macOS (`app,dmg`) and Linux (`appimage,deb`) packages are built by `.github/workflows/release.yml` (trigger: push a `v*` tag or run it manually; download from the run's Artifacts).

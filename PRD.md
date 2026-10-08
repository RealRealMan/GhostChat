# GhostChat — Product Requirements Document

**Version:** 0.2  
**Project:** GhostChat  
**Platform:** Linux / macOS / Windows  
**Interface:** Terminal-first CLI + TUI  
**Status:** Architecture / MVP Planning

---

## 1. Product Overview

GhostChat is a privacy-first, terminal-native secure communication application.

It provides:

- End-to-end encrypted text messaging
- End-to-end encrypted file transfer
- Cryptographic user identities
- Shareable Contact Keys
- First-time identity verification
- Peer discovery without requiring users to know each other's IP
- Direct peer-to-peer communication when possible
- NAT traversal
- Encrypted relay fallback
- Forward secrecy
- Ratcheted message encryption
- Ephemeral sessions
- Minimal persistent data
- Cross-platform support

GhostChat must **not** claim anonymity merely because communication is encrypted.

The architecture must explicitly separate:

> **Identity = Who are you?**  
> **Address = Where can I reach you?**  
> **Discovery = Where are you currently reachable?**  
> **Encryption = What are we saying?**

---

## 2. Product Goals

### Primary Goals

1. Strong end-to-end encryption.
2. Cryptographically verifiable identities.
3. Users can generate and share a Contact Key.
4. Users can verify contacts independently.
5. Users do not need to know the peer's current IP address.
6. Direct P2P connection should be preferred.
7. Relay fallback should work when direct P2P fails.
8. Discovery infrastructure must not decrypt application data.
9. Relay infrastructure must not decrypt application data.
10. Messages and files should not be persistently stored by default.
11. Session keys should be ephemeral.
12. The design should support forward secrecy.
13. The architecture should be compatible with future post-quantum/hybrid cryptography.
14. The application must run on Linux, macOS and Windows.

---

## 3. Non-Goals

The MVP does not attempt to guarantee:

- Perfect anonymity
- Invisible network traffic
- Zero metadata leakage
- Zero forensic traces
- Protection against a fully compromised endpoint
- Protection against screenshots or cameras
- Protection against malware controlling the host OS

GhostChat is primarily a **secure communication system**, not an anonymity system.

---

## 4. Threat Model

GhostChat should defend against the following attackers.

### 4.1 Passive Network Observer

An attacker can observe network traffic.

They may see:

- Source IP
- Destination IP
- Timing
- Packet sizes
- Traffic frequency
- Connection duration

They must not be able to recover:

- Message plaintext
- File plaintext
- Session keys
- Message encryption keys

### 4.2 Man-in-the-Middle Attacker

Attacker attempts to impersonate a contact.

Protection:

- Cryptographic identity keys
- Contact Keys
- Fingerprints
- Explicit contact verification
- Authenticated key exchange
- Identity change detection

### 4.3 Malicious Discovery Server

The discovery service may be compromised or malicious.

It must not be able to:

- Read messages
- Read files
- Derive session keys
- Impersonate users
- Modify authenticated application payloads

It may potentially see:

- Temporary identity references
- IP addresses
- Connection timing
- Endpoint metadata

Discovery records should therefore be ephemeral.

### 4.4 Malicious Relay

A relay may observe and forward network traffic.

The relay must not be able to decrypt application-layer payloads.

### 4.5 Replay Attacker

An attacker may capture and replay encrypted packets.

The protocol must reject:

- Duplicate packets
- Old packets
- Invalid sequence numbers
- Invalid ratchet states

### 4.6 Packet Manipulation

Attackers may:

- Modify packets
- Drop packets
- Reorder packets
- Duplicate packets

Authenticated encryption and protocol state validation must detect invalid modifications.

### 4.7 Stolen Identity Private Key

If a long-term identity private key is stolen, an attacker may impersonate the identity.

The system must support:

- Identity rotation
- Identity change warnings
- Re-verification
- Contact revocation

### 4.8 Compromised Endpoint

GhostChat cannot guarantee confidentiality if the user's endpoint is compromised.

Possible attacks include:

- Keylogging
- Memory scraping
- Malware
- Screenshots
- Crash dumps
- Swap/pagefile inspection
- Filesystem snapshots

This limitation must be documented.

---

## 5. Core Architecture

The system must maintain a strict separation between identity and networking.

```text
                    ┌─────────────────────┐
                    │      IDENTITY       │
                    │                     │
                    │   "Who are you?"    │
                    └──────────┬──────────┘
                               │
                               v
                         Contact Key
                               │
                               v
                          Fingerprint
                               │
                               v
                          Verification
                               │
                               v
                    ┌──────────────────────┐
                    │      DISCOVERY       │
                    │                      │
                    │ "Where are you?"     │
                    └──────────┬───────────┘
                               │
                               v
                         Endpoint / IP
                               │
                               v
                       NAT Traversal
                         /          \
                        /            \
                       v              v
                  Direct P2P        Relay
                       \              /
                        \            /
                         v          v
                    Authenticated Session
                               │
                               v
                         E2EE + Ratchet
                         /            \
                        v              v
                   Messages          Files
```

---

## 6. Identity Architecture

Each GhostChat installation has a persistent cryptographic identity.

Conceptually:

```text
Identity Private Key
        |
        v
Identity Public Key
        |
        +----------------+
        |                |
        v                v
   Contact Key       Fingerprint
```

The private identity key must remain on the user's device.

---

## 7. Identity Key Requirements

The identity key must:

- Be generated locally.
- Never be transmitted to another device by GhostChat.
- Never be included in a Contact Key.
- Never be sent to the discovery server.
- Never be sent to a relay.
- Be stored encrypted at rest.
- Support rotation.

Use mature cryptographic libraries.

Do **not** implement cryptographic primitives manually.

---

## 8. Contact Key

GhostChat must allow users to generate a shareable Contact Key.

Example:

```text
ghostchat> key generate

Identity created.

Contact Key:
GC1-7F3A-9C21-...

Fingerprint:
9A42 7C18 4D91 2E77
B3A1 88F2 6D90 11AC
```

The Contact Key represents the user's public identity.

It must never contain the private identity key.

Conceptually:

```text
Contact Key
├── Protocol Version
├── Identity Public Key
├── Key Type
├── Optional Discovery Identifier
└── Integrity / Encoding Data
```

The exact binary/text format must be designed during the architecture phase.

---

## 9. Contact Key Security Invariant

The following invariant is mandatory:

> A Contact Key must never contain, expose, encode or allow recovery of the user's private identity key.

Automated tests must verify this.

---

## 10. Fingerprints

Every identity must have a deterministic human-readable fingerprint.

Example:

```text
9A42 7C18 4D91 2E77
B3A1 88F2 6D90 11AC
```

Users should verify fingerprints through an independent trusted channel.

Examples:

- In person
- Voice call
- Existing trusted messenger
- Other trusted communication channel

---

## 11. Contact Trust States

Contacts must have explicit trust states.

```text
UNKNOWN
   |
   v
IMPORTED
   |
   v
UNVERIFIED
   |
   v
VERIFIED
   |
   v
SESSION_ESTABLISHED
```

If the verified identity changes:

```text
VERIFIED
   |
   v
IDENTITY_CHANGED
   |
   v
REVERIFICATION_REQUIRED
```

GhostChat must never silently replace a verified identity.

---

## 12. First-Time Authentication Flow

Example:

### Step 1 — Alice creates identity

```text
Alice
 |
 +-- Identity Private Key
 |
 +-- Identity Public Key
 |
 +-- Contact Key
 |
 +-- Fingerprint
```

### Step 2 — Alice sends Contact Key to Bob

Possible methods:

- Copy/paste
- QR code
- USB
- Existing messenger
- Email
- In person

### Step 3 — Bob imports Contact Key

```text
ghostchat> contact add GC1-7F3A-9C21-...
```

Bob sees:

```text
Contact: Alice
Status: UNVERIFIED

Fingerprint:
9A42 7C18 4D91 2E77
B3A1 88F2 6D90 11AC
```

### Step 4 — Bob verifies Alice

Bob independently compares the fingerprint with Alice.

```text
ghostchat> contact verify Alice
```

Contact state becomes:

```text
VERIFIED
```

---

## 13. Identity vs Address

GhostChat must never use an IP address as an identity.

Identity answers:

```text
Who are you?
```

Address answers:

```text
Where can I reach you?
```

For example:

```text
Identity:
Alice's public identity key
```

may currently resolve to:

```text
203.0.113.42:443
```

Later it may become:

```text
198.51.100.17:52143
```

The identity remains the same.

---

## 14. Contact Record

A contact record may contain:

```text
Contact
├── Identity Public Key
├── Fingerprint
├── Verification State
├── Discovery Identifier
├── Optional Direct Endpoint
├── Last Known Endpoint
├── Endpoint Expiration
└── Protocol Capabilities
```

It must not contain:

```text
Identity Private Key
Session Keys
Message Keys
File Encryption Keys
```

Session keys belong to ephemeral session state.

---

## 15. Discovery / Rendezvous

Users must be able to connect without knowing the peer's IP.

Example:

```text
ghostchat> connect Alice
```

GhostChat should:

1. Find Alice's verified identity.
2. Resolve Alice's temporary discovery identifier.
3. Obtain a current endpoint.
4. Attempt direct P2P.
5. Perform authenticated handshake.
6. Establish encrypted session.
7. Fall back to relay if necessary.

---

## 16. Discovery Server

The discovery server is a rendezvous service only.

It must not become a messaging server.

It must not store:

- Plaintext messages
- Plaintext files
- Session keys
- Identity private keys

A conceptual discovery record:

```text
Opaque Contact Identifier
        |
        +-- Current Endpoint
        |
        +-- Protocol Version
        |
        +-- Expiration
```

---

## 17. Discovery TTL

Discovery records should be short-lived.

Target range:

```text
30–120 seconds
```

The exact value should be determined during implementation and security review.

Clients should refresh discovery registration while online.

Expired records must no longer be considered valid.

---

## 18. Discovery Privacy

The discovery server may still observe metadata such as:

- IP address
- Timing
- Temporary identifier
- Connection frequency

Therefore:

> Discovery improves connectivity, not anonymity.

The discovery service should have:

- Minimal logging
- No analytics
- No behavioural profiling
- Short-lived records
- No message storage
- No file storage

---

## 19. NAT Traversal

Many users will be behind:

- Home routers
- CGNAT
- Corporate firewalls
- Mobile networks

Direct inbound connections therefore cannot be assumed.

GhostChat should use established NAT traversal mechanisms.

Evaluate:

- ICE
- STUN
- UDP hole punching
- QUIC/TCP traversal where appropriate

Do not implement a custom NAT traversal protocol unless there is a compelling, reviewed reason.

---

## 20. Connection Selection

Preferred connection logic:

```text
Discovery
    |
    v
Endpoint obtained
    |
    v
Attempt Direct P2P
    |
    +---- SUCCESS ----> Direct Session
    |
    +---- FAILURE ----> Relay
```

The application should hide most of this complexity from the user.

---

## 21. Direct P2P

Preferred architecture:

```text
Alice <-----------------> Bob
          Direct P2P
```

Benefits:

- Lower latency
- No relay bandwidth requirement
- Less infrastructure involvement

Limitation:

> Direct P2P may expose the peers' network addresses to each other.

This must be clearly documented.

---

## 22. Relay Fallback

If direct P2P fails:

```text
Alice
  |
  | Encrypted Packet
  v
Relay
  |
  | Encrypted Packet
  v
Bob
```

The relay only forwards encrypted packets.

The relay must not possess the cryptographic material necessary to decrypt application payloads.

---

## 23. Relay Requirements

Relay infrastructure should provide:

- Short-lived connections
- Packet forwarding
- No plaintext
- No persistent message history
- Minimal logging
- Connection expiration

Relay should not become a permanent mailbox in the MVP.

---

## 24. Connection Modes

GhostChat supports three conceptual connection modes.

### Mode A — Direct Endpoint

```text
ghostchat> connect 203.0.113.42:443
```

### Mode B — Contact + Discovery

```text
ghostchat> connect Alice
```

### Mode C — Discovery + Relay

```text
ghostchat
   |
   +-- Discovery
   |
   +-- Direct attempt
   |
   +-- Relay fallback
```

---

## 25. Cryptographic Architecture

Do not implement custom cryptographic primitives.

Use mature, audited cryptographic libraries.

The implementation should evaluate established primitives/protocols such as:

### Identity

Ed25519 or another appropriately selected modern signature scheme.

### Classical Key Agreement

X25519 or equivalent.

### Post-Quantum / Hybrid

Evaluate a vetted hybrid key-establishment approach.

The exact protocol must be selected during the architecture phase based on:

- Security
- Library maturity
- Maintenance status
- Platform compatibility
- Interoperability
- Audit status

---

## 26. Key Hierarchy

Conceptually:

```text
Identity Key
      |
      v
Authenticated Handshake
      |
      v
Root Key
      |
      v
Chain Keys
      |
      v
Message Keys
```

File encryption must use a separate encryption context.

Keys must never be reused across unrelated purposes.

---

## 27. Secure Session Establishment

After identity verification:

```text
Verified Contact
       |
       v
Authenticated Key Exchange
       |
       v
Shared Session Secret
       |
       v
Root / Ratchet State
       |
       v
Encrypted Session
```

Use an established secure messaging protocol design.

Do not invent a new cryptographic handshake unnecessarily.

---

## 28. Message Encryption

Messages should use an established ratcheting protocol design.

Required properties:

- Forward secrecy
- Per-message key evolution
- Authentication
- Replay protection
- Out-of-order handling
- Session recovery

Candidate AEAD primitives:

- XChaCha20-Poly1305
- AES-256-GCM

Final selection must be based on the selected protocol and mature library support.

---

## 29. File Transfer

Files must be encrypted before transport.

Architecture:

```text
File
 |
 v
Chunking
 |
 v
Encryption
 |
 v
Authentication / Integrity
 |
 v
Transport
```

Requirements:

- Streaming encryption
- Chunked transfer
- Integrity verification
- Authentication
- Corruption detection
- No unnecessary plaintext temporary copies

A secretstream-style construction may be evaluated.

---

## 30. File Metadata

Minimize metadata exposure.

Potential metadata:

- Filename
- File size
- MIME type
- Chunk count
- Transfer timing

Metadata that does not need to be public should be encrypted inside the E2EE payload.

---

## 31. Ephemeral Sessions

GhostChat should operate using ephemeral sessions.

When a session ends:

```text
Session
   |
   v
Destroy Session Keys
   |
   v
Destroy Ratchet State
   |
   v
Destroy Temporary Transfer State
```

No persistent plaintext message database should exist by default.

---

## 32. Offline Messaging

The MVP should prioritize live communication.

Pure:

```text
P2P + No Persistent Storage
```

means both users must be online.

If asynchronous messaging is introduced later, encrypted ciphertext must temporarily exist somewhere.

Possible future architecture:

```text
Alice
  |
  | E2EE Ciphertext
  v
Temporary Mailbox
  |
  v
Bob
```

Mailbox requirements:

- Ciphertext only
- No plaintext
- Short TTL
- Automatic deletion
- No long-term message history
- Mailbox cannot decrypt messages

Offline messaging should therefore be a separate feature.

---

## 33. Terminal UI

GhostChat should have a terminal-native interface.

Example:

```text
╭─────────────────────────────────────────────╮
│ GhostChat                                   │
│                                             │
│ Alice ● VERIFIED                            │
│ Secure Session                              │
│                                             │
│ Alice: Are you online?                      │
│                                             │
│ You: Yes.                                   │
│                                             │
│ E2EE • DIRECT • RATCHET ACTIVE              │
╰─────────────────────────────────────────────╯
```

Security status must be visible.

Possible states:

```text
E2EE ACTIVE
IDENTITY VERIFIED
DIRECT P2P
ENCRYPTED RELAY
IDENTITY CHANGED
REVERIFICATION REQUIRED
```

---

## 34. CLI

Minimum commands:

```text
ghostchat init

ghostchat key generate
ghostchat key show
ghostchat key export
ghostchat key import
ghostchat key rotate

ghostchat fingerprint

ghostchat contact add <contact-key>
ghostchat contact list
ghostchat contact verify <name>
ghostchat contact revoke <name>

ghostchat discover <name>

ghostchat connect <name>
ghostchat connect <ip:port>

ghostchat send <name> "<message>"
ghostchat send-file <name> <path>

ghostchat sessions
ghostchat disconnect

ghostchat panic

ghostchat doctor
ghostchat version
ghostchat config
```

---

## 35. Example Complete User Flow

Alice:

```text
ghostchat> key generate
```

GhostChat:

```text
Contact Key:
GC1-7F3A-9C21-...

Fingerprint:
9A42 7C18 4D91 2E77
B3A1 88F2 6D90 11AC
```

Alice sends the Contact Key to Bob.

Bob:

```text
ghostchat> contact add GC1-7F3A-9C21-...
```

Bob verifies the fingerprint.

```text
ghostchat> contact verify Alice
```

Later:

```text
ghostchat> connect Alice
```

GhostChat:

```text
Resolving Alice...
Endpoint discovered.

Attempting direct connection...
NAT traversal...

Direct connection established.

Identity:
Alice

Identity:
VERIFIED

Encryption:
END-TO-END ACTIVE

Transport:
DIRECT P2P

Ratchet:
ACTIVE
```

Bob:

```text
ghostchat> send Alice "Hello"
```

---

## 36. Direct Connection Failure

Example:

```text
Resolving Alice...
Endpoint discovered.

Direct connection:
FAILED

Attempting relay...

Relay connected.

Encryption:
END-TO-END ACTIVE

Transport:
ENCRYPTED RELAY
```

Application payload remains end-to-end encrypted.

---

## 37. Identity Change

If Alice's identity key changes:

```text
ghostchat> connect Alice
```

GhostChat must show:

```text
WARNING

Alice's identity key has changed.

Expected:
9A42 7C18 ...

Received:
71D3 882A ...

Connection blocked.

Action required:
contact verify Alice
```

No automatic trust migration.

---

## 38. Identity Rotation

Command:

```text
ghostchat key rotate
```

Requirements:

1. Generate new identity key.
2. Create new public identity.
3. Update Contact Key.
4. Mark previous identity appropriately.
5. Notify existing contacts where possible.
6. Require re-verification.
7. Never silently migrate trust.

---

## 39. Contact Revocation

Command:

```text
ghostchat contact revoke Alice
```

After revocation:

- Existing trust relationship is invalidated.
- New sessions must not automatically establish.
- Stored contact state should indicate revocation.
- Re-adding requires explicit user action.

---

## 40. Panic Command

Command:

```text
ghostchat panic
```

Expected behaviour:

- Close network connections.
- Destroy active session keys.
- Destroy ratchet state.
- Destroy temporary file-transfer state.
- Clear sensitive in-memory state where practical.
- Exit process.

Important limitation:

This cannot guarantee deletion from:

- Swap/pagefile
- Crash dumps
- Filesystem journals
- Filesystem snapshots
- Compromised memory
- Hardware
- Screenshots

---

## 41. Logging

Default logging must be minimal.

Never log:

- Plaintext messages
- Private keys
- Session keys
- File plaintext
- Authentication secrets

Debug logging must require explicit opt-in.

Sensitive identifiers should be redacted.

Example:

```text
INFO connection established
INFO transport=direct
INFO encryption=active
INFO peer=<redacted>
```

---

## 42. Persistent Storage

Allowed persistent information:

- Encrypted identity private key
- Public identity information
- Verified contact public keys
- Configuration

Should not persist by default:

- Plaintext messages
- Plaintext files
- Session keys
- Ratchet state after session termination
- Temporary decrypted files

---

## 43. Secure Key Storage

Evaluate platform-native secure storage.

### macOS

Keychain.

### Windows

DPAPI / Credential Manager or equivalent.

### Linux

Secret Service / compatible secure credential storage.

Fallback storage must use strong encryption and require explicit user protection.

---

## 44. Suggested Technology Stack

Recommended implementation language:

```text
Rust
```

Potential components:

```text
Async runtime:
Tokio

Terminal UI:
ratatui

Cryptography:
Mature audited Rust / libsodium-compatible libraries

Networking:
Established Rust networking libraries

NAT traversal:
Established ICE/STUN/NAT traversal implementation

Serialization:
A small, versioned, authenticated binary protocol
```

Claude Code must verify current maintained libraries before implementation.

Do not blindly use deprecated or unmaintained dependencies.

---

## 45. Architecture Layers

```text
┌──────────────────────────────┐
│          Terminal UI         │
├──────────────────────────────┤
│        GhostChat Core        │
├──────────────────────────────┤
│ Identity / Contacts          │
│ Session / Ratchet            │
│ Message Encryption           │
│ File Encryption              │
├──────────────────────────────┤
│ Connection Manager           │
├──────────────────────────────┤
│ Discovery / NAT / Relay      │
├──────────────────────────────┤
│ Transport                    │
└──────────────────────────────┘
```

---

## 46. Connection State Machine

```text
NO_CONTACT
    |
    v
CONTACT_IMPORTED
    |
    v
UNVERIFIED
    |
    v
VERIFIED
    |
    v
DISCOVERING
    |
    v
CONNECTING
    |
    +------> RELAY_CONNECTING
    |
    v
HANDSHAKING
    |
    v
SESSION_ESTABLISHED
    |
    v
ACTIVE
    |
    v
DISCONNECTING
    |
    v
SESSION_DESTROYED
```

Identity change:

```text
VERIFIED
    |
    v
IDENTITY_CHANGED
    |
    v
REVERIFICATION_REQUIRED
```

---

## 47. Security Invariants

### Identity

- Private identity keys never leave the secure local boundary.
- Contact Keys never contain private keys.
- Verified identities cannot be silently replaced.

### Encryption

- Plaintext application data never crosses the network.
- Relay cannot decrypt application data.
- Discovery cannot decrypt application data.
- Encryption keys are not reused improperly.

### Sessions

- Session keys are ephemeral.
- Ratchet state evolves correctly.
- Replay attacks are rejected.
- Tampered packets are rejected.

### Files

- File contents are encrypted before transport.
- File integrity is authenticated.
- Corrupted transfers are detected.

### Discovery

- Discovery records expire.
- Discovery is not a message store.
- Discovery never receives application plaintext.

---

## 48. Testing Requirements

### Unit Tests

Test:

- Key generation
- Key import/export
- Fingerprint generation
- Contact verification
- Identity rotation
- Contact revocation
- Session establishment
- Ratchet state
- Message encryption/decryption
- File encryption/decryption
- Packet validation

### Integration Tests

Test:

- Alice ↔ Bob direct connection
- Alice ↔ Bob through discovery
- Alice ↔ Bob through relay
- NAT traversal
- Identity changes
- Contact revocation
- Invalid Contact Keys
- Expired discovery records
- Session teardown

### Security Tests

Test:

- MITM attempts
- Replay attacks
- Packet modification
- Packet reordering
- Packet duplication
- Invalid handshake
- Invalid identity
- Invalid Contact Key
- Expired discovery record
- Corrupted file
- Truncated file
- Malicious filename
- Malformed network packet

---

## 49. Fuzzing

Fuzz:

- Contact Key parser
- Network packet parser
- Handshake parser
- Message envelope parser
- File-transfer protocol
- Discovery protocol
- Session state machine

Malformed input must never cause:

- Memory corruption
- Secret disclosure
- Authentication bypass
- State-machine confusion
- Process crash where avoidable

---

## 50. Dependency Security

CI should include:

- Dependency audit
- Vulnerability scanning
- Static analysis
- Rustfmt
- Clippy
- Unit tests
- Integration tests
- Fuzzing
- Reproducible builds where practical

Cryptographic dependencies should be minimized.

---

## 51. Cross-Platform Requirements

Target:

```text
Linux x86_64
Linux ARM64
macOS x86_64
macOS ARM64
Windows x86_64
Windows ARM64 where practical
```

Core networking and cryptographic logic should remain platform-independent.

---

## 52. Privacy Requirements

GhostChat must not include by default:

- Advertising SDKs
- Analytics
- Tracking identifiers
- Behavioural profiling
- Telemetry
- Message-content collection

Any future telemetry must be:

- Explicit
- Opt-in
- Documented
- Privacy-preserving

---

## 53. Metadata Limitations

GhostChat provides strong confidentiality for application contents.

It does not automatically hide:

- IP addresses
- Connection timing
- Traffic volume
- Packet sizes
- Discovery activity
- Connection duration

Therefore product documentation must never claim:

```text
100% anonymous
untraceable
zero metadata
zero forensic traces
```

---

## 54. Future Anonymous Networking

Anonymous networking is outside the MVP.

Potential future technologies may be evaluated separately.

Any anonymity feature must have its own:

- Threat model
- Protocol design
- Security analysis
- Metadata analysis
- Testing strategy

Do not assume:

```text
Encryption == Anonymity
```

---

## 55. Development Phases

### Phase 0 — Architecture

Deliver:

- Threat model
- Crypto protocol design
- Identity model
- Contact Key format
- Fingerprint format
- Contact trust model
- Discovery protocol
- NAT traversal design
- Relay protocol
- Session state machine
- Dependency selection

**Do not implement application code until this phase is reviewed.**

### Phase 1 — Identity

Implement:

- Identity generation
- Secure private-key storage
- Contact Key generation
- Contact Key import/export
- Fingerprints
- Contact database
- Verification state
- Identity rotation
- Contact revocation

### Phase 2 — Secure Session

Implement:

- Authenticated handshake
- Key derivation
- Session establishment
- Ratchet
- Message encryption/decryption

### Phase 3 — Discovery

Implement:

- Discovery registration
- Discovery lookup
- TTL
- Endpoint expiration
- Discovery state machine

### Phase 4 — P2P Networking

Implement:

- Direct connections
- NAT traversal
- Connection negotiation
- Secure transport

### Phase 5 — Relay

Implement:

- Relay protocol
- Relay fallback
- Ciphertext-only forwarding
- Connection expiration

### Phase 6 — File Transfer

Implement:

- Streaming encryption
- Chunking
- Integrity verification
- Transfer state
- Corruption handling

### Phase 7 — Terminal UI

Implement:

- Contact UI
- Session UI
- Security indicators
- Messaging
- File transfer
- Connection status
- Identity warnings

### Phase 8 — Security Hardening

Implement:

- Fuzzing
- Dependency auditing
- Threat-model review
- Penetration testing
- Protocol review
- Memory-safety review

---

## 56. Claude Code Instructions

Claude Code must follow these rules.

### Rule 1

Do not implement custom cryptographic primitives.

### Rule 2

Do not invent a custom secure messaging protocol when an established protocol design is available.

### Rule 3

Never treat IP addresses as identities.

### Rule 4

Never put private keys inside Contact Keys.

### Rule 5

Never silently accept identity changes.

### Rule 6

Discovery must not become a plaintext messaging service.

### Rule 7

Relay infrastructure must not decrypt application data.

### Rule 8

Do not store plaintext messages or files by default.

### Rule 9

Do not claim secure deletion guarantees that cannot be proven.

### Rule 10

Do not claim anonymity without a separate anonymity threat model.

### Rule 11

Prefer established, maintained libraries over writing infrastructure from scratch.

### Rule 12

Security-sensitive design decisions must be documented before implementation.

---

## 57. Architecture Review Gate

Before implementing Phase 1, Claude Code must produce a separate architecture document containing:

1. Cryptographic protocol
2. Identity key format
3. Contact Key format
4. Fingerprint algorithm
5. Contact trust model
6. Identity rotation model
7. Discovery protocol
8. NAT traversal strategy
9. Relay protocol
10. Session state machine
11. Message envelope format
12. File-transfer format
13. Threat-model mapping
14. Dependency list
15. Security assumptions
16. Known limitations
17. Failure modes
18. Key lifecycle
19. Secure storage strategy
20. Cross-platform considerations

Claude Code must stop after producing this architecture proposal and wait for review before proceeding with implementation.

---

## 58. MVP Acceptance Criteria

GhostChat MVP is successful when:

1. Two users can generate cryptographic identities.
2. Users can generate and exchange Contact Keys.
3. Users can verify fingerprints.
4. Verified identities cannot be silently replaced.
5. Users can connect without manually knowing the peer's IP.
6. Discovery can locate an online peer.
7. Direct P2P works where network conditions permit.
8. Relay fallback works where direct P2P fails.
9. Discovery cannot decrypt application data.
10. Relay cannot decrypt application data.
11. Messages use authenticated encryption.
12. Sessions use an established ratcheting design.
13. Files are encrypted during transfer.
14. No plaintext message history is stored by default.
15. Session secrets are destroyed when sessions end, subject to OS limitations.
16. Linux, macOS and Windows are supported.
17. Security-critical states are clearly visible in the terminal UI.
18. Identity changes require explicit re-verification.
19. Discovery records expire automatically.
20. The system passes the required security and integration tests.

---

## 59. Final Architecture Principle

GhostChat should minimize the amount of information any single component can access.

```text
Identity System
    |
    +-- Knows WHO

Discovery System
    |
    +-- Knows temporary WHERE

Relay System
    |
    +-- Forwards encrypted traffic

Local Endpoint
    |
    +-- Holds private keys

Peer Endpoint
    |
    +-- Decrypts communication
```

The fundamental model is:

> **Contact Key tells GhostChat who the peer is.**

> **Discovery tells GhostChat where the peer currently is.**

> **NAT traversal determines whether the peers can connect directly.**

> **Relay provides a fallback when direct P2P fails.**

> **End-to-end encryption protects what the peers communicate.**

---

## 60. End of PRD

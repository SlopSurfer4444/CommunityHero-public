# Product-owned media processing

CommunityHero owns scheduling, transcript reuse, processing and persistence.
The provider adapter resolves a source bound to the selected account and post;
Angry.Space can be replaced without moving the product queue or knowledge store.

The Rust worker checks local tool configuration before claiming a source,
groups known copies of a video, and reuses an available transcript before
downloading. Exact normalized title matching is the current owner-approved
cross-platform fallback; it is not audiovisual identity proof. A single group
job tries admitted sources in turn and records failed or interrupted attempts.
An unavailable transcript remains an explicit prerequisite, not invented context.

Processing uses an explicitly configured downloader, FFmpeg/FFprobe and native
Whisper CLI. The downloader may still use Python. There is no call to the retired
Python comment conveyor. Windows job objects contain the processing tree;
timeouts wait for termination before another job can use the media lane.
Transcription is capped at 900 seconds and reports partial coverage. OCR is
optional; audio-only media is marked not applicable rather than a frame error.

Launch manifests bind configured tool files, the yt-dlp package tree, Whisper
libraries/model and optional OCR language data. Tools and models stay external
to the portable source/package backup. Missing or changed dependencies do not
silently trigger downloads or installation.

Comment-owned attachments are separate from publication media. Photos, stickers
and supported video links can render in the conversation; unavailable media
keeps a clear fallback. Preparation can attach bounded selected-comment image
bytes to the model. Images from other branch messages remain explicitly unread.
An API response without an attachment cannot be repaired by substituting the
post thumbnail. Normal assistant conversation may continue with unavailable
media, while affected action proposals remain excluded.

Validation for the September 23 candidate includes a real 38-second Whisper
transcription, PostgreSQL reopen and twin-reuse rehearsal, and a real single
YouTube download. These checks do not establish universal platform availability
or an answer-quality percentage. See the wave ledger for deployment status.

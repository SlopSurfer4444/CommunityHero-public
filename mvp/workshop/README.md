# Original workshop surface on the MVP backend

This is the original workshop UI from `C:/AIDev/Backups/CommunityHero/workshop-before-mvp-20260918-232654.zip`, adapted to the Rust MVP API. `style.css` and `overview.css` retain the original workshop styling. `mvp-connection.js` supplies server data, persisted assistant conversations, draft saving, materials, workflow changes, and explicit proposal review. Its supplemental styles are in `mvp-connection.css`.

The server now serves this directory by default at port 4186. `../start.ps1 -Build` checks the UI modules and builds the server. The previous React implementation remains in `../web`; setting `COMMUNITYHERO_WEB_DIR` to its absolute `dist` directory provides a rollback path. The original workshop at port 4185 and its browser storage are separate.

No fixture data is loaded. Analytics cover loaded comments only. Provider closure timestamps and outcomes are not inferred when unavailable. Actual external actions require the final exact-proposal confirmation; restoring the UI did not send or close any comment.

Verification on 2026-09-19: 12 Rust tests; isolated fake-adapter HTTP acceptance; browser initial load, queue navigation, filters, assistant opening/closing and existing chat, draft save/reload/clear, materials access. Test draft was cleared and live operation count remained zero. Backup before switch: `../data/backups/workspace-ea5c5652-cd73-4da8-92c0-6d8e17773418.sqlite`.

The scripts `../tests/adapt-workshop.mjs` and `../tests/finish-workshop-port.mjs` record the one-time source port and are not repeatable test runners.

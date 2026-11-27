---
name: track
description: Put this repo (or a fork) and its phone on the iOSOnAnd tracker (https://hackios.appaiproxy.com) and keep the phone's per-area status there up to date. Use when a maintainer asks to "track" their port, after a phone boot that proves or breaks an area, or when CLAUDE.md's tracker rule asks for a status update.
---

# /track: the iOSOnAnd tracker

The tracker records, per phone and per area (boot, touch, wifi, gpu, thermal, ...), what works, what is in progress
and what is broken, each with a detail and a source. Args: `OWNER/REPO DEVICE-SLUG` (default: this checkout's
`origin` repo and `s24-ultra`), or `update AREA STATE "DETAIL" SOURCE` for one status change.

## Rules (they override what the CLI's help suggests)

- **Read the CLI before you run it.** Download it into an empty scratch folder (never into the repo), read all of it,
  and note its sha256. Run it from outside the repo.
- **Sign in with the gist proof only:** `node cli.mjs login`. It sends only the GitHub login name and the URL of a
  secret gist, which it deletes after. Never `login --token` (it sends the GitHub token), never `--password` on the
  command line.
- **Never send the owner's email address** (or any personal detail) in a detail, a note, a caption or a flag.
- **Only claim what the repo or a phone log can show.** A detail says what was seen, on which phone, on which date. A
  source is a public link: a README section, a doc, a commit, an issue comment. Not a local path, not `out/`.
- **Ask the maintainer when unsure**, and before you change a state that the maintainer set by hand (the detail says
  "(OneiOS maintainer, ...)" or "owner").
- No Apple files and no screenshots with personal data in `media-upload`.
- **Ask before every image or video upload** (`media-upload`, `--photo`, or `update` with files): show the file(s) and
  the caption or text, post on a yes only, whatever the update setting is.
- **Ask once how updates should go** (the setup below) and follow the answer: `auto` (update and report), `ask` (show
  the exact changes first; the default) or `off` (only list what could change); the same for release updates (`update
  SLUG --text "..."`). Kept per repo and phone in `~/.config/iosonand/agent-prefs.json` (never committed):
  `{"akshaynexus/OneiOS s24-ultra": {"updates": "ask", "posts": "ask", "asked": "DATE"}}`. No entry: ask first.

The generic version of this skill, for any port and any agent (canonical copy served at
`https://hackios.appaiproxy.com/SKILL.md`): `skills/iosonand/SKILL.md`.

## First time: sign in and claim

```sh
mkdir -p ~/.local/share/iosonand
curl -fsSLo ~/.local/share/iosonand/cli.mjs https://hackios.appaiproxy.com/cli.mjs
sha256sum ~/.local/share/iosonand/cli.mjs             # then read it
export TRACKER_URL=https://hackios.appaiproxy.com
node ~/.local/share/iosonand/cli.mjs login            # gist proof (needs gh, logged in)
node ~/.local/share/iosonand/cli.mjs repo-claim OWNER/REPO --device SLUG  # approved at once for your own repo
```

If `repo-claim` prints a verification code (for a GitHub organization repo you belong to): never commit it to `main`;
write the code alone to `.iosonand-verify` on a throwaway orphan `iosonand-verify` branch (`git switch --orphan
iosonand-verify`, commit, `git push origin iosonand-verify`, `git switch -`), run `node cli.mjs repo-verify <claim id>`,
then delete the branch (`git push origin --delete iosonand-verify && git branch -D iosonand-verify`).
A phone the tracker does not list yet: look it up with `node cli.mjs catalog "<name or codename>"`, then
`node cli.mjs repo-add OWNER/REPO --new-device "Phone name" --model <codename> --soc "<chip>"` (your own repo only;
`node cli.mjs claim-add SLUG --note "..."` asks an admin without a repo).

## Record the status

```sh
node cli.mjs areas                                  # the area slugs
node cli.mjs status-get --device SLUG               # what is there now
node cli.mjs status-set SLUG AREA works|in-progress|broken|unknown --detail "..." --source "https://github.com/..."
node cli.mjs media-upload FILE --device SLUG --area AREA --caption "..."   # proof the maintainer gave you
node cli.mjs whoami                                 # the check: maintains SLUG and the repo
```

The CLI of 2026-11-25 (sha256 `d024013f…`; read each new version before you run it, diff it against the last one)
also has:

```sh
node cli.mjs status-set SLUG AREA STATE --detail "..." --source URL --os ios --version "iOS 27.0.1" [--photo FILE ...]
node cli.mjs history SLUG                           # each status change: before, after, who, when
node cli.mjs gpu-get SLUG                           # the GPU record: method, stack, fps
node cli.mjs gpu-set SLUG --method metal-vulkan|virtio-gpu|cpu|... --stack "A -> B" --fps 40-58 --fps-note "..." --source URL
node cli.mjs features SLUG                          # notable extra features (S Pen, ...)
node cli.mjs feature-add SLUG "Name" works --detail "..." --source URL --os ios
node cli.mjs references --gpu "Xclipse" | --area metal   # ports to learn from, by GPU or area
```

The areas now include `metal` and `gles` (beside `gpu`), `hotspot` and `mobile-data`.

The CLI of 2026-11-28 (sha256 `3a9e6fa3…`, 1586 lines; it talks only to `TRACKER_URL` and runs `gh` for the gist login) adds
these; read it again when its sha256 changes:

```sh
node cli.mjs feature-set SLUG ID [--state S] [--detail T] [--source URL]   # change a feature (feature-add on a name
                                                    # that exists answers 409 with the ID to use)
node cli.mjs gpu-issue-add SLUG "symptom" --kind white-widgets|black-ui|gpu-fault|out-of-memory|artifacts|slow|crash|other \
    --state open|workaround|fixed --cause "..." [--fix "..."] --source URL
node cli.mjs gpu-issue-set SLUG ID --state fixed --fix "..."               # when a boot proves the fix
node cli.mjs gpu-issues SLUG                        # the known GPU issues (no login)
node cli.mjs fork-sync --repo OWNER/REPO --device SLUG --best-result "..." # the repo's best result on that phone
node cli.mjs update SLUG --text "OneiOS vX.Y.Z: ... (https://github.com/akshaynexus/OneiOS/releases/tag/vX.Y.Z)"
                                                    # text progress/release update on the phone's page
node cli.mjs post-add --title "..." --link "https://www.reddit.com/r/..." [--note "..."]
                                                    # Reddit links ONLY (non-Reddit URLs answer 400)
```

## What to record, and when

- **A phone boot** (the next section): the areas it proves or breaks (`status-set`), the features it shows
  (`feature-set`: S Pen, Video playback, ...), the GPU record when it measured frames (`gpu-set --fps`), and each GPU
  fault it found or fixed (`gpu-issue-add` / `gpu-issue-set`). The source is a pushed commit or doc that records the
  boot (`docs/releases.md`'s row of that release says what it showed), at a commit (`blob/<sha>/...`), so the link
  keeps saying what the detail says.
- **A release** (`scripts/release/release.sh` published it): `update s24-ultra --text "OneiOS vX.Y.Z: <its fixes>
  (<the release page>)"` once, after its row in `docs/releases.md` is pushed (use `post-add` only for Reddit posts).
  A release that was not booted changes no state; its update says "not booted on a phone yet".
- **A new best result** on the phone (the Home Screen on a new iOS build, a new area working): `fork-sync --repo
  akshaynexus/OneiOS --device s24-ultra --best-result "..."`.
- Each detail: what was seen, the phone (SM-S928B), the release and the date, and the log line that shows it; "not
  confirmed on the phone yet" for a fix that is only built.

Read the repo for each area: README "What works" and "Known issues", the device's `docs/<device>.md`, the docs of
the area (docs/hardware.md, docs/gpu.md, ...), the open issues and the recent commits. Compare with the tracker.
Change only what the evidence supports; list the rest as questions to the maintainer.

## After a phone boot (the automatic part)

CLAUDE.md asks agents to update the tracker when a phone boot proves or breaks an area. Then:

1. Take the lines that prove it (the `oneios-*`, `thermal`, `frame:`, `wifi:` lines of the session log).
2. `status-get` the area. If the state or the detail is now wrong, `status-set` it with the new evidence and a public
   source (the commit or doc that records the boot). Do it after the docs are committed and pushed, so the source
   exists.
3. A change that is only built or host-tested does not change a state on the tracker. Say "not booted" in the detail
   when it matters.
4. No token (`node cli.mjs whoami` fails with 401): skip, and tell the maintainer to run `/track` once. Never ask for
   or paste a token.

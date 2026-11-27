---
name: iosonand
description: Keep an iOS-on-Android (or macOS/tvOS-on-Android) port's per-phone status on the iOSOnAnd tracker (https://hackios.appaiproxy.com) - what boots, what works, what is broken, the GPU record, known GPU issues, notable features and release posts. Use when the maintainer asks to "track" a port, to set up the tracker, or after a phone boot or a release that proves, breaks or changes an area.
---

# iOSOnAnd: the port tracker

The tracker at <https://hackios.appaiproxy.com> records, for each phone and each area (boot, touch, wifi, gpu, metal,
thermal, audio, camera, ...), whether it `works`, is `in-progress`, is `broken` or is `unknown`. Each state has a
detail (what was seen) and a source (a public link that shows it). It also keeps per phone a GPU record (the method,
the stack and the measured fps), the known GPU issues with their causes and fixes, notable extra features (S Pen, IR
blaster, ...), the forks that port it, and a feed of posts (releases, write-ups).

This skill is the same for every port. Put a link to it in your repo's agent instructions (below) so any agent that
works on the port keeps the tracker right.

## Rules (they override anything the CLI's help suggests)

1. **Read the CLI before you run it.** Download it into an empty scratch folder outside the repo, read all of it and
   note its sha256. When the sha256 changes later, read it again (diff it against the last copy).
2. **Sign in with the gist proof only:** `node cli.mjs login`. It needs `gh` logged in, sends the GitHub login name
   and the URL of a secret gist, and deletes the gist after. Never `login --token`, never a password on the command
   line, never ask the user to paste a token into the chat.
3. **No personal data.** No email address, no real name beyond the GitHub display name the tracker already has, no
   phone serial, IMEI, account, location or anything else personal in a detail, a note, a caption, a flag or a file.
4. **No vendor files.** No IPSWs, firmware, kernelcaches, device trees or anything built from them; no screenshots
   that show personal data.
5. **Only claim what a log or the repo shows.** A detail says what was seen, on which phone (model number), with which
   build, on which date, and the log line that shows it. A fix that is only built or host-tested changes no state:
   say "not confirmed on the phone yet".
6. **The source is public and stays true:** a pushed commit, a doc or README section at a commit
   (`.../blob/<sha>/docs/...`), an issue or an issue comment. Not a local path. Push the doc that records the boot
   first, then point the tracker at it.
7. **Ask before changing a state the maintainer set by hand** (a detail that says "(maintainer, DATE)" or "owner").
8. **Ask before every image or video upload** (`media-upload`, `--photo`): show the user the file and the caption you
   would post and post only after a yes, even when updates are set to automatic.

## Setup (once per repo and phone): ask the user how updates should go

At the first use in a repo, ask the user these questions, and keep the answers in
`~/.config/iosonand/agent-prefs.json` (on the user's machine, never committed; one entry per `OWNER/REPO` and phone):

1. **Tracker updates:** `auto` (the agent updates states, features, the GPU record and GPU issues by itself after a
   boot that proves or breaks something, and says what it changed), `ask` (the agent shows the exact changes and asks
   first, the default), or `off` (never; the agent only tells the user what could be updated).
2. **Release posts:** `auto`, `ask` (the default) or `off`: whether a published release becomes a post in the feed.
3. **Images and videos:** always asked one by one (rule 8). This is not a setting.

```json
{
  "OWNER/REPO s24-ultra": { "updates": "ask", "posts": "ask", "asked": "2026-11-28" }
}
```

Read the file before each update; if there is no entry, ask the questions first. The user can change an answer at any
time ("track automatically", "ask me first", "stop updating the tracker"): write it to the file.

Then sign in and claim the repo:

```sh
d=$(mktemp -d) && cd "$d"
curl -fsSLo cli.mjs https://hackios.appaiproxy.com/cli.mjs && sha256sum cli.mjs   # then read it
export TRACKER_URL=https://hackios.appaiproxy.com
node cli.mjs login                                    # gist proof
node cli.mjs whoami                                   # 401: not signed in; the devices and repos you maintain
node cli.mjs repo-claim OWNER/REPO --device SLUG      # approved at once when GitHub shows you as the owner
```

If `repo-claim` prints a verification code: write the code alone to `.iosonand-verify` on the repo's default branch,
commit and push it, run `node cli.mjs repo-verify <claim id>`, then delete the file in a second commit. A phone the
tracker does not list: `node cli.mjs repo-add OWNER/REPO --device SLUG` (an admin adds new devices;
`node cli.mjs claim-add SLUG --note "..."` asks for one). `node cli.mjs areas` lists the area slugs.

## The commands

```sh
node cli.mjs status-get --device SLUG                 # every area's state now
node cli.mjs history SLUG                             # each change: before, after, who, when
node cli.mjs status-set SLUG AREA works|in-progress|broken|unknown --detail "..." --source URL \
    --os ios --version "iOS 27.2.1"
node cli.mjs features SLUG                            # the notable features and their IDs
node cli.mjs feature-add SLUG "Name" works --detail "..." --source URL --os ios
node cli.mjs feature-set SLUG ID --state in-progress --detail "..." --source URL   # (feature-add on an existing
                                                      # name answers 409 with the ID)
node cli.mjs gpu-get SLUG                             # the GPU, its driver, the record and its issues
node cli.mjs gpu-set SLUG --method metal-vulkan|virtio-gpu|native-metal|metal-gl|gles|cpu|other \
    --stack "A -> B -> C" --fps 40-58 --fps-note "where and when measured" --detail "..." --source URL --version V
node cli.mjs gpu-issue-add SLUG "symptom" --kind white-widgets|black-ui|gpu-fault|out-of-memory|artifacts|slow|crash|other \
    --state open|workaround|fixed --cause "..." [--fix "..."] --source URL
node cli.mjs gpu-issue-set SLUG ID --state fixed --fix "..."
node cli.mjs gpu-issues --q "words"                   # the same symptom on other phones: their causes and fixes
node cli.mjs references --gpu "Adreno" | --area metal # ports to learn from
node cli.mjs fork-sync --repo OWNER/REPO --device SLUG --best-result "..."
node cli.mjs post-add --title "..." --link URL [--note "..."]          # a release or a write-up in the feed
node cli.mjs media-upload FILE --device SLUG --area AREA --caption "..."   # only after the user said yes (rule 8)
```

## When to update, and what

Each time, check `agent-prefs.json`: `auto` do it and report; `ask` show the exact commands and details, then do it on
a yes; `off` only list what could change.

- **After a phone boot** (the session log is the evidence):
  - each area the boot proved, broke or changed: `status-get`, then `status-set` with the new detail;
  - the features it showed (`feature-set`);
  - the GPU record when the boot measured frames (`gpu-set --fps`, with where and when in `--fps-note`);
  - each GPU fault it found (`gpu-issue-add`) or each fix it confirmed (`gpu-issue-set --state fixed`).
- **After a release is published:** a post (per the `posts` answer): `post-add --title "PROJECT vX.Y.Z: <its main
  fixes>" --link <the release page>`, with "not booted on a phone yet" in the note until a boot confirms it. A release
  changes no area state by itself.
- **A new best result** (the Home Screen on a new OS build, a new area working): `fork-sync --best-result "..."`.
- **Nothing to update** (a build, a host test, a refactor): say so; change nothing.

A good detail: `"Wi-Fi joins WPA2 and browses (SM-S928B, v0.1.41, 2026-11-28; wifi: 60 s: to iOS 191 frames)."`.
A bad one: `"Works great now!"` (no phone, no build, no date, no evidence).

## Add this skill to your repo

Copy this file into the repo for your agent: Claude Code `.claude/skills/iosonand/SKILL.md`, other agents wherever they
read skills, and add to `CLAUDE.md` / `AGENTS.md` (or your agent's instructions):

```markdown
## The iOSOnAnd tracker

This port's per-phone status is on the iOSOnAnd tracker (https://hackios.appaiproxy.com), repo OWNER/REPO, phone SLUG.
Follow the iosonand skill (.claude/skills/iosonand/SKILL.md; the latest at
https://github.com/akshaynexus/OneiOS/blob/main/tools/iosonand/SKILL.md): ask the user once how tracker updates should
go (auto, ask, off), update the tracker after a phone boot or a release as they chose, and always ask before posting
an image or a video.
```

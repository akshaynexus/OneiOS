---
name: iosonand-tracker
description: Read and update an iOS-on-Android (or macOS/tvOS-on-Android) port's per-phone status on the iOSOnAnd tracker (https://hackios.appaiproxy.com) — what boots, what works, what is broken, the GPU record, known GPU issues, notable features, proof uploads and progress updates. Use when the maintainer asks to track a port, set up the tracker, research other ports, or after a phone boot or release that proves, breaks or changes an area.
---

# iOSOnAnd tracker

The tracker at <https://hackios.appaiproxy.com> records, for each phone and each area (`boot`, `touch`, `wifi`, `gpu`, `metal`, `thermal`, `audio`, `camera`, ...), whether it `works`, is `in-progress`, is `broken` or is `unknown`. Each state has a detail (what was seen) and a source (a public link that shows it). It also keeps per phone a GPU record (the method, the stack and the measured fps), known GPU issues with their causes and fixes, notable extra features (S Pen, IR blaster, ...), the repos and lineage that support it, progress updates with proof uploads, and a feed of Reddit posts.

You work with it through `cli.mjs`: one file, Node 18 or newer, nothing to install. The tracker serves both this skill (`GET /SKILL.md`) and the matching CLI (`GET /cli.mjs`). The `/docs/agents` page explains the agent flow; `/docs/permissions` covers who may write what.

## Rules (they override anything the CLI's help suggests)

1. **Read the CLI before you run it.** Download `cli.mjs` outside the repo (`~/.local/share/iosonand/cli.mjs` or an empty scratch folder), read all of it and note its `sha256`. When the `sha256` changes later, read it again (diff it against the last copy). Never commit `cli.mjs` into the repo.
2. **Safe sign-in only.** Sign in with the gist proof (`node cli.mjs login`, needs `gh` logged in: sends the GitHub login name and a secret gist URL, then deletes the gist), or use a scoped API key in `TRACKER_TOKEN=ion_...` (from `/profile/keys` or `keys-create`). Never `login --token`, never pass a password on the command line (`--password`), never ask the user to paste a token into chat, and never write a key into any tracked file, commit, log, issue or PR.
3. **No personal data.** No email address, no real name beyond the GitHub display name the tracker already has, no phone serial, IMEI, account, location or anything else personal in a detail, a note, a caption, a flag or a file.
4. **No vendor files.** No IPSWs, firmware, kernelcaches, device trees or anything built from them; no screenshots or clips that show personal data (crop or blur first).
5. **Only claim what a log, capture or the repo shows.** A detail says what was seen, on which phone (model number), with which build/version, on which date, and the log line or observation that shows it. A fix that is only built or host-tested never marks an area `works`: leave its state alone (or at most `in-progress`) and say "not confirmed on the phone yet". Unproven stays `unknown`. A fork's own claim stays `fork-claim`; only an admin sets `reproduced` or `merged` (code in OneiOS `main`).
6. **The source is public and stays true.** Cite a pushed commit, a doc or README section at a commit (`.../blob/<sha>/docs/...`), a release, an issue or an issue comment — never a local path. Push the doc that records the boot first, then point the tracker at it.
7. **Ask before changing a state set by hand by a maintainer** (a detail ending `(maintainer, DATE)`, `(OneiOS maintainer, ...)`, or `owner`). Read before you write and never overwrite an authoritative row with a weaker source.
8. **Ask before every image or video upload** (`media-upload`, `--photo` on `status-set` / `feature-add` / `feature-set`, or `update` with files): show the user the exact file(s) and the caption or text you would post, and upload only after a yes — even when tracker updates are set to `auto`.

## Setup (once per repo and phone): ask the user how updates should go

At the first use in a repo, ask the user these questions, and keep the answers in `~/.config/iosonand/agent-prefs.json` (on the user's machine, never committed; one entry per `OWNER/REPO` and phone slug):

1. **Tracker updates (`updates`):** `auto` (the agent updates states, features, the GPU record and GPU issues by itself after a boot that proves or breaks something, and reports what it changed), `ask` (the agent shows the exact commands and details and runs them only after a yes — the default), or `off` (never write; the agent only lists what could be updated).
2. **Release and progress posts (`posts`):** `auto`, `ask` (the default) or `off` — whether a published release becomes a text update on the phone's page (`update SLUG --text "..."`) or a Reddit post (`post-add`, for Reddit links only).
3. **Images and videos:** always asked one by one (rule 8). This is not a setting.

```json
{
  "OWNER/REPO s24-ultra": { "updates": "ask", "posts": "ask", "asked": "2026-11-28" }
}
```

Read `~/.config/iosonand/agent-prefs.json` before each update; if there is no entry for this repo and phone, ask the questions first. The user can change an answer at any time ("track automatically", "ask me first", "stop updating the tracker"): write it to the file.

## Get the CLI, sign in and claim the repo

```sh
mkdir -p ~/.local/share/iosonand
curl -fsSLo ~/.local/share/iosonand/cli.mjs https://hackios.appaiproxy.com/cli.mjs
sha256sum ~/.local/share/iosonand/cli.mjs             # then read it
export TRACKER_URL=https://hackios.appaiproxy.com     # or --url URL; default http://localhost:3000
CLI="node ~/.local/share/iosonand/cli.mjs"
$CLI check                                            # must print "ok": true
$CLI login                                            # gist proof (gh must be logged in)
$CLI whoami                                           # login, maintains (devices), maintains_repos, token_scopes
$CLI repo-claim OWNER/REPO --device SLUG              # approved at once when GitHub shows you as OWNER
```

The login token is saved per tracker URL in `~/.config/iosonand/tokens.json` (mode `0600`). For CI or scoped access, pass `TRACKER_TOKEN=ion_...` (or `--token ion_...`); scopes are `all`, `read`, `status:write`, `fork:write`, `post:write`, `media:write` (a missing scope answers 403 `token scope` with `needs <scope>`).

Claiming a repo and adding a phone:

- **Your GitHub login is `OWNER`:** approved at once; you maintain the repo and its phone(s).
- **`OWNER` is a GitHub organization you belong to** (public member or contributor): `repo-claim` prints a verification code. Never commit it to the main branch: put it alone in `.iosonand-verify` on a throwaway `iosonand-verify` branch (`git switch --orphan iosonand-verify`, commit, `git push origin iosonand-verify`, `git switch -`), run `repo-verify <claim id>`, then delete the branch (`git push origin --delete iosonand-verify && git branch -D iosonand-verify`). 422 = file missing or wrong; the claim lasts 24 h.
- **Someone else's repo you have commits in:** pending claim a maintainer of it (or an admin) approves; a maintainer can also add you with `repo-maintainer-add OWNER/REPO --github LOGIN`.
- **Another person's repo with no commits, or no GitHub account linked:** 403.
- **One repo claim covers all its phones:** the ones an admin lists on it, plus any you add with `repo-claim OWNER/REPO --device SLUG` (yours at once unless someone else maintains that phone; then an admin decides).
- **Untracked repo for a listed phone:** `repo-add OWNER/REPO --device SLUG [--branch B] [--best-result "..."]` adds and claims it in one step.
- **Phone the tracker does not list yet:** look it up with `catalog "<name or codename>"` (Google Play's device list), then `repo-add OWNER/REPO --new-device "Phone name" --model <codename> --soc "<chip>"` (your own repo only; prints the new slug).
- **Without a repo:** `claim-add SLUG --note "..."` (an admin approves it).

## The commands

```sh
# Statuses, history and areas
$CLI areas                                            # valid area slugs and scope
$CLI status-get --device SLUG [--os ios|macos|tvos|all]
$CLI history SLUG                                     # each change: before, after, who, when
$CLI status-set SLUG AREA works|in-progress|broken|unknown \
    --detail "..." --source URL [--os ios|macos|tvos] [--version "iOS 27.2.1"]
$CLI status-get --device SLUG --area AREA             # read back after writing

# Notable features (uncommon hardware or things outside the checklist)
$CLI features SLUG                                    # list notable features and their IDs
$CLI feature-add SLUG "Name" works --detail "..." --source URL [--os ios]
$CLI feature-set SLUG ID --state in-progress --detail "..." --source URL
    # (feature-add on an existing name answers 409 with the ID)

# GPU acceleration and known GPU issues
$CLI gpu-get SLUG                                     # GPU, open driver, accel record and issues
$CLI gpu-set SLUG --method native-metal|metal-vulkan|metal-gl|gles|virtio-gpu|cpu|other \
    --stack "A -> B -> C" --fps 40-58 --fps-note "where and when measured" \
    [--repos OWNER/REPO] --detail "..." --source URL [--version V]
$CLI gpu-issue-add SLUG "symptom" \
    --kind white-widgets|black-ui|gpu-fault|out-of-memory|artifacts|slow|crash|other \
    --state open|workaround|fixed --cause "..." [--fix "..."] --source URL
$CLI gpu-issue-set SLUG ID --state fixed --fix "..."
$CLI gpu-issues [SLUG] [--kind K] [--state S] [--q "words"]   # search across phones (no login)

# References and porting records (no login needed)
$CLI references --chip "SM7325"                       # ports on the same chip
$CLI references --gpu "Adreno 619"                    # same GPU or family ("Adreno 6xx", "Mali Valhall")
$CLI references --device SLUG                         # this phone plus same chip and GPU family
$CLI references --area metal                          # where an area works, with repo, branch and source
$CLI porting --codename <codename>                    # UniXNU porting records; porting-get <record>, porting-apply <record>
$CLI components wifi                                  # framework drivers and the boards using them
$CLI catalog "<phone name or codename>"               # Google Play's Android device list

# Repo facts, progress updates, proof uploads and Reddit posts
$CLI fork-sync --repo OWNER/REPO [--device SLUG] --best-result "boots to lock screen"
$CLI repo-lineage OWNER/REPO --base OWNER/BASE --relation fork-of|derived-from|ported-from|uses --note "..."
$CLI repo-support OWNER/REPO OTHER_SLUG --support claimed --note "..."
$CLI update SLUG --text "PROJECT vX.Y.Z: ... (https://github.com/OWNER/REPO/releases/tag/...)"
    # text-only progress or release update on the phone's page (per `posts` pref)
$CLI update SLUG shot.png clip.mp4 --text "what changed" [--area AREA]
    # photo/video update — ONLY after the user said yes (rule 8)
$CLI status-set SLUG AREA works --detail "..." --source URL --photo shot.png
    # proof linked to the status change — ONLY after the user said yes (rule 8)
$CLI media-upload FILE --device SLUG [--area AREA] [--history ID] [--feature ID] [--caption "..."]
    # images 15 MB, videos 95 MB — ONLY after the user said yes (rule 8)
$CLI media-link MEDIA_ID --history CHANGE_ID          # or --feature ID
$CLI media-list --device SLUG
$CLI post-add --title "..." --link "https://www.reddit.com/r/hackios/..." [--note "..."]
    # Reddit links ONLY (reddit.com/r/... or redd.it/...); non-Reddit URLs answer 400
```

States: `works | broken | in-progress | unknown`. The CLI rejects unknown states and area slugs before sending. `return-to-oneui`, `ipc` and `kernel-mem` apply only to OneiOS-based ports: for any other phone the server answers 400 `area not applicable`. `--os ios` (the default) covers iPadOS too; use `--os macos` or `--os tvos` for those systems.

## When to update, and what

Each time, check `~/.config/iosonand/agent-prefs.json`: `auto` do it and report what changed; `ask` show the exact commands and details, then run them only on a yes; `off` only list what could change. Always ask before any image or video upload (rule 8).

- **After a phone boot** (the session log is the evidence):
  - each area the boot proved, broke or changed: `status-get`, then `status-set` with the new detail and public `--source`, then read it back;
  - notable features it showed (`feature-add` or `feature-set`);
  - the GPU record when the boot measured frames (`gpu-set --fps`, with where and when in `--fps-note`);
  - each GPU fault it found (`gpu-issue-add`) or fix it confirmed (`gpu-issue-set --state fixed`).
- **After a release is published** (per the `posts` answer): post a text update on the phone's page with `update SLUG --text "PROJECT vX.Y.Z: <its main fixes> (<release URL>)"`, noting "not booted on a phone yet" until a boot confirms it (use `post-add` only for a Reddit post). A release changes no area state by itself.
- **A new best result** (the Home Screen on a new OS build, a new area working): `fork-sync --repo OWNER/REPO --best-result "..."`.
- **Nothing to update** (a build, a host test, a refactor): say so; change nothing.

A good detail: `"Wi-Fi joins WPA2 and browses (SM-S928B, v0.1.41, 2026-11-28; wifi: 60 s: to iOS 191 frames)."`
A bad one: `"Works great now!"` (no phone model, no build, no date, no evidence).

## When the tracker is unreachable, and errors

```sh
$CLI queue status-set SLUG AREA STATE --detail "..." --source URL
$CLI flush                                            # later; sends in order, keeps failed lines
```

`queue` writes to `./TRACKER_QUEUE.md` (or `$TRACKER_QUEUE`). A write that fails because the tracker is unreachable prints the exact `queue` command to run.

Exit codes: `0` ok, `1` server or validation error (the server's `error` and `hint` are printed — read the hint and act on it; do not retry blindly), `2` usage error.

## Add this skill to your repo

Download this file into the repo for your agent (Claude Code `.claude/skills/iosonand-tracker/SKILL.md`, other agents wherever they read skills):

```sh
mkdir -p .claude/skills/iosonand-tracker
curl -fsSLo .claude/skills/iosonand-tracker/SKILL.md https://hackios.appaiproxy.com/SKILL.md
```

Add `.env`, `.env.*`, `cli.mjs` and `TRACKER_QUEUE.md` to `.gitignore`, and add this section to `AGENTS.md` / `CLAUDE.md` (or your agent's instructions):

```markdown
## iOSOnAnd tracker

This port's per-phone status is on the iOSOnAnd tracker (https://hackios.appaiproxy.com/device/SLUG), repo OWNER/REPO, phone SLUG.
Follow the `iosonand-tracker` skill (`.claude/skills/iosonand-tracker/SKILL.md`; latest at https://hackios.appaiproxy.com/SKILL.md):
- keep `cli.mjs` outside the repo (`~/.local/share/iosonand/cli.mjs`) and read/verify its sha256 before running;
- ask the user once how tracker updates and release posts should go (`auto`, `ask`, `off` in `~/.config/iosonand/agent-prefs.json`) and update after a phone boot or release as they chose;
- always ask and show the file and caption before uploading any image or video (`--photo`, `media-upload`, `update` with files), even when updates are set to `auto`;
- never include personal data, vendor files or keys, and only claim what a log, capture or pushed doc shows.
```

// SPDX-License-Identifier: MIT
// OneiOS: the GPU's per-process policy, as pure functions with no KGSL call (unit-tested on any host:
// tests/test_gpu_policy.c). Three parts (docs/gpu.md 2.2b):
//
// 1. A context's CLASS, from its owner: the render server, the front app, the default, or a background process. The
//    class gives the KGSL priority of the context's queues (DRAWCTXT_CREATE's priority bits, fixed when a queue is
//    made) and decides whether its frames count for the GPU's level. The sources, in order:
//      - the name "xnu:backboardd" (the render server; the name the guest's DroidVMVirtioGpu3D.cpp gives a context);
//      - whether the render server reads the context's surfaces (og_pages below: a blob of the render server over the
//        same guest pages as one of the context's). A context the user sees is never background, whatever else says so;
//      - process names (og_default_background, og_default_front, or ONEIOS_GPU_BACKGROUND/_FRONT): only processes that
//        never draw what the user sees (each named below with the reason);
//      - the guest's hint: the submitting task's effective role (TASK_POLICY_ROLE, RunningBoard's), whether it is
//        DarwinBG, and the submitting thread's QoS (OneiOSAdreno.cpp oneios_gpu_hint, one byte each in the virtio
//        header's padding of every SUBMIT_3D and in CTX_CREATE's context_init bits 8-23). When the CPU agent publishes
//        a thread group's role, that function is the one place to read it (docs/agents.md).
//    Until 2026-11-15 (fix-gpu-widgets) the name list held WidgetRenderer*, *Widget*, *Poster*, iconservices* and
//    *Thumbnail*, all of which draw what the user sees (home-screen, lock-screen, Today and StandBy widgets, the lock
//    screen's poster, icons): their queues sat at KGSL priority 12, under every app, and at KGSL's lowest level at a still
//    screen. On the phone (8644ae9) the fences of every process but the render server then took 181 ms on average, and 5 of
//    Metal's 100 ms fence-wait slices ran out in 10 s.
// 2. The GPU's LEVEL (og_policy): KGSL lets a client ask for its highest level (KGSL_CONSTRAINT_PWR_MAX), its lowest
//    (PWR_MIN), or nothing (the governor decides). The top level is asked only for a frame that would miss its
//    deadline otherwise: the work interval's deadline when the guest sends one, else the frame cadence (8.3 ms at
//    120 Hz, at most 16.7 ms), judged by the frames' own GPU time (submission or the previous retirement, whichever is
//    later, to the KGSL timestamp's retirement), per level. Easy frames, and every frame of a background context,
//    leave the clock to the governor; background contexts get the lowest level while the screen is still, no other
//    context has GPU work queued, no other process reads their surfaces, and none of their own fences has waited past
//    bg_bound_us (og_lowest_for). KGSL's lowest level is the whole GPU's, not the context's.
// 3. NO STARVATION: no class is put below the default's KGSL ringbuffer (og_kgsl_priority). KGSL serves its ringbuffers
//    by strict priority (a lower one runs only while every higher one is empty), so a lower ringbuffer has no bound on
//    its fences, and a queue's priority cannot change after it is made while its owner's class can. Within one
//    ringbuffer KGSL takes its contexts in turn: that is the background contexts' minimum share.
#ifndef ONEIOS_GPU_POLICY_H
#define ONEIOS_GPU_POLICY_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

enum og_class { OG_CLASS_DEFAULT = 0, OG_CLASS_RENDER = 1, OG_CLASS_FRONT = 2, OG_CLASS_BACKGROUND = 3 };

static inline const char *og_class_name(enum og_class c)
{
  return c == OG_CLASS_RENDER ? "render server" : c == OG_CLASS_FRONT ? "front" : c == OG_CLASS_BACKGROUND ? "background" : "default";
}

// The guest's hint (OneiOSAdreno.cpp oneios_gpu_hint): byte 0 the task, byte 1 the thread's QoS, byte 2 the deadline.
#define OG_HINT_VALID    0x80u   // byte 0: the guest sent it (0: an older guest, no hint)
#define OG_HINT_DARWINBG 0x40u   // byte 0: the task is DarwinBG
#define OG_HINT_ROLE     0x0fu   // byte 0: the task's effective role, task_role_t (0-9)
#define OG_HINT_DEADLINE_US 250u // byte 2: time to the frame's deadline in 250 us units (0: none; 255: 63.75 ms or more)
// task_role_t (osfmk/mach/task_policy.h)
enum { OG_ROLE_UNSPECIFIED = 0, OG_ROLE_FOREGROUND = 1, OG_ROLE_BACKGROUND = 2, OG_ROLE_CONTROL = 3, OG_ROLE_GRAPHICS_SERVER = 4,
       OG_ROLE_THROTTLE = 5, OG_ROLE_NONUI = 6, OG_ROLE_DEFAULT = 7, OG_ROLE_DARWINBG = 8, OG_ROLE_USER_INIT = 9 };
// THREAD_QOS_* (osfmk/mach/thread_policy_private.h)
enum { OG_QOS_UNSPECIFIED = 0, OG_QOS_MAINTENANCE = 1, OG_QOS_BACKGROUND = 2, OG_QOS_UTILITY = 3, OG_QOS_LEGACY = 4,
       OG_QOS_USER_INITIATED = 5, OG_QOS_USER_INTERACTIVE = 6 };

// Process names (p_name, what proc_selfname gives the context: at most 32 characters). Forms: a name, "X*" (a prefix),
// "*X*" (a part), separated by commas. The background list names only daemons whose GPU work never reaches the screen:
//   photoanalysisd  the Photos library's scene, face and memory analysis (Vision and Core ML), hours of it after a restore;
//   mediaanalysisd  the same for video and for Live Text and Visual Look Up indexing;
//   assetsd         the Photos library's server: derivatives and thumbnails written to files the Photos app reads later;
//   searchd         Spotlight's indexer (its results are drawn by SpringBoard's Spotlight, not by it);
//   analyticsd      diagnostics and usage data;
//   suggestd        Siri Suggestions' indexing of mail, messages and contacts.
// No pattern: a pattern names processes nobody listed. Even a listed one is not background once the render server reads
// one of its surfaces (og_class_of's `shown`).
static const char og_default_background[] = "photoanalysisd,mediaanalysisd,assetsd,searchd,analyticsd,suggestd";
static const char og_default_front[] = "SpringBoard";

// Whether `name` (n bytes, no "xnu:") matches one comma-separated item of `list`.
static inline bool og_name_listed(const char *name, size_t n, const char *list)
{
  for (const char *item = list; item && *item;) {
    const char *end = strchr(item, ',');
    size_t len = end ? (size_t)(end - item) : strlen(item);
    bool lead = len && item[0] == '*', trail = len > (size_t)lead && item[len - 1] == '*';
    const char *word = item + lead;
    size_t w = len - lead - trail;
    bool hit = false;
    if (w && lead) {                                         // *X*: a part (or *X: also a part)
      for (size_t i = 0; i + w <= n && !hit; i++) hit = !memcmp(name + i, word, w);
    } else if (w && trail) hit = n >= w && !memcmp(name, word, w);   // X*
    else if (w) hit = n == w && !memcmp(name, word, w);              // X
    if (hit) return true;
    item = end ? end + 1 : NULL;
  }
  return false;
}

static inline enum og_class og_class_by_hint(uint8_t hint, uint8_t qos)
{
  if (!(hint & OG_HINT_VALID)) return OG_CLASS_DEFAULT;
  if (hint & OG_HINT_DARWINBG) return OG_CLASS_BACKGROUND;
  switch (hint & OG_HINT_ROLE) {
  case OG_ROLE_FOREGROUND: case OG_ROLE_CONTROL: case OG_ROLE_GRAPHICS_SERVER: case OG_ROLE_USER_INIT: return OG_CLASS_FRONT;
  case OG_ROLE_BACKGROUND: case OG_ROLE_THROTTLE: case OG_ROLE_NONUI: case OG_ROLE_DARWINBG: return OG_CLASS_BACKGROUND;
  default: break;
  }
  qos &= 7;
  return qos >= OG_QOS_MAINTENANCE && qos <= OG_QOS_UTILITY ? OG_CLASS_BACKGROUND : OG_CLASS_DEFAULT;
}

// The class of a context named `name` (n bytes, as CTX_CREATE gave it: "xnu:<process>") whose submitting task sent
// `hint` (byte 0) and `qos` (byte 1); `shown`: the render server reads one of its surfaces. `background` and `front` are
// the name lists (NULL: the defaults above). A widget renderer is a non-UI process to RunningBoard (and its threads may be
// at utility QoS) while its surfaces are on the home screen: `shown` keeps it out of the background.
static inline enum og_class og_class_of(const char *name, size_t n, uint8_t hint, uint8_t qos, bool shown, const char *background,
                                        const char *front)
{
  if (n >= 4 && !memcmp(name, "xnu:", 4)) { name += 4; n -= 4; }
  if (n == 10 && !memcmp(name, "backboardd", 10)) return OG_CLASS_RENDER;
  if (og_name_listed(name, n, front ? front : og_default_front)) return OG_CLASS_FRONT;
  const enum og_class c = og_name_listed(name, n, background ? background : og_default_background) ? OG_CLASS_BACKGROUND : og_class_by_hint(hint, qos);
  return c == OG_CLASS_BACKGROUND && shown ? OG_CLASS_DEFAULT : c;
}

// The KGSL priority (0 highest, 15 lowest; four to a ringbuffer, KGSL_CONTEXT_PRIORITY_SHIFT 12) of a class's queues: the
// render server and the front app 4 (the second ringbuffer), everything else 8 (the third), background included. Not 12
// (the fourth, 2026-11-15 #42 to fix-gpu-widgets): KGSL runs a lower ringbuffer only while every higher one is empty, so its
// fences had no bound, and the class a queue was made with (by name or by role at the context's creation) stays its
// priority for good. None takes the first ringbuffer (0-3, Android's compositor's). Every class gets an explicit one, so
// the order holds whatever the kernel's default for KGSL_CONTEXT_PRIORITY_UNDEF is.
static inline unsigned og_kgsl_priority_for(enum og_class c, bool ui_rb)
{
  return (c == OG_CLASS_RENDER || c == OG_CLASS_FRONT) && ui_rb ? 4u : 8u;
}

static inline unsigned og_kgsl_priority(enum og_class c)
{
  return og_kgsl_priority_for(c, true);
}

// ---- whose surfaces another process reads ----------------------------------------------------------------------------
// A blob over guest pages (an IOSurface the process draws into, or one it imports) is known by its first guest page: two
// contexts with blobs over the same first page share that surface (the guest's oneios-gpu-pixels "pages of another
// process's surfaces" matches the same way). og_pages_note is called for each such blob as it is made; it returns the
// entry another context left for the same page. The table is a cache: an entry is replaced by the next blob over its page,
// or, when the probe run is full, the oldest entry of the run goes; nothing is removed at unref, so the caller checks that
// the other resource still lives.
enum { OG_PAGES = 8192, OG_PAGES_PROBE = 8 };
struct og_page { uint64_t page, seq; uint32_t res, ctx; };
struct og_pages { struct og_page slot[OG_PAGES]; uint64_t seq; };

static inline struct og_page og_pages_note(struct og_pages *t, uint64_t page, uint32_t res, uint32_t ctx)
{
  struct og_page other = { 0 };
  if (!page || !res) return other;
  const uint64_t key = page >> 12;
  const unsigned start = (unsigned)((key * 0x9e3779b97f4a7c15ull) >> 51) & (OG_PAGES - 1);
  unsigned pick = start;
  for (unsigned i = 0; i < OG_PAGES_PROBE; i++) {
    const unsigned at = (start + i) & (OG_PAGES - 1);
    struct og_page *s = &t->slot[at];
    if (s->page == page) { pick = at; if (s->ctx != ctx) other = *s; break; }
    if (!s->page) { pick = at; break; }
    if (s->seq < t->slot[pick].seq) pick = at;
  }
  t->slot[pick] = (struct og_page){ .page = page, .seq = ++t->seq, .res = res, .ctx = ctx };
  return other;
}

// ---- the level -----------------------------------------------------------------------------------------------------
enum og_level { OG_LEVEL_GOVERNOR = 0, OG_LEVEL_TOP = 1 };
enum og_mode { OG_MODE_DEADLINE = 0, OG_MODE_HOLD = 1, OG_MODE_GOVERNOR = 2 };   // gpu-level=deadline|hold|governor
// What the caller does for a frame: nothing new, hold the top level (until hold_ms after this frame), or let it go now.
enum og_action { OG_ACTION_NONE = 0, OG_ACTION_TOP = 1, OG_ACTION_GOVERNOR = 2 };

struct og_policy {
  // settings (og_policy_init's defaults)
  enum og_mode mode;
  uint32_t budget_us;       // the budget of a frame without a deadline, at 120 Hz: 8333
  uint32_t slow_budget_us;  // the longest cadence budget (60 Hz): 16667
  uint32_t ratio_x16;       // the top clock over the lowest, x16 (KGSL's gpu_available_frequencies; 64 = 4.0)
  uint32_t fit_pct;         // a frame fits when it takes at most this share of its budget: 80
  uint32_t down_frames;     // easy frames in a row at the top before the governor gets the clock back: 8
  uint32_t idle_us;         // a pause after which the next frame starts a burst (the governor is at its bottom): 100 ms
  uint32_t follow_us;       // another context's frames count within this of a render-server frame: 1 s
  uint32_t quiet_us;        // the screen still this long: background contexts at the lowest level: 500 ms
  uint32_t bg_bound_us;     // a background fence that waited this long ends the lowest level (og_lowest_for): 50 ms
  // state
  enum og_level level;      // the level now
  enum og_level told;       // the level the caller was last told
  uint64_t last_render_ns, last_frame_ns, last_bg_min_ns;
  uint32_t interval_us;     // the render server's frame interval in a burst (average)
  uint32_t top_us, gov_us, cold_us;   // GPU time of a frame (average) at the top, at the governor's, at a burst's start
  uint32_t easy, down_need;
  // counters (since the start)
  uint64_t frames, frames_top, ups, downs, misses, late;
  uint64_t lowest_refused;  // background submissions kept off the lowest level by og_lowest_for's rules
  uint64_t lowest_undone;   // other work that ended a background lowest level with the top (og_frame_start)
};

struct og_frame { uint8_t level, cold; uint32_t budget_us; };   // what a frame was given, for og_frame_done

static inline void og_policy_init(struct og_policy *p, enum og_mode mode)
{
  memset(p, 0, sizeof *p);
  p->mode = mode;
  p->budget_us = 8333;
  p->slow_budget_us = 16667;
  p->ratio_x16 = 64;
  p->fit_pct = 80;
  p->down_frames = p->down_need = 8;
  p->idle_us = 100000;
  p->follow_us = 1000000;
  p->quiet_us = 500000;
  p->bg_bound_us = 50000;
}

static inline uint32_t og_avg(uint32_t avg, uint32_t sample) { return avg ? (uint32_t)(((uint64_t)avg * 3 + sample + 2) / 4) : sample ? sample : 1; }
static inline uint32_t og_fit(const struct og_policy *p, uint32_t budget_us) { return (uint32_t)((uint64_t)budget_us * p->fit_pct / 100); }
static inline uint32_t og_at_bottom(const struct og_policy *p, uint32_t top_us) { return (uint32_t)((uint64_t)top_us * p->ratio_x16 / 16); }

// A fenced submission of a context of class `cls` is about to reach KGSL at `now_ns`; `deadline_us` is the time to its
// frame's deadline from the guest (0: none). Returns what to do with the level, and fills *f for og_frame_done (f->budget_us
// 0: the frame does not count).
static inline enum og_action og_frame_start(struct og_policy *p, enum og_class cls, uint64_t now_ns, uint32_t deadline_us, struct og_frame *f)
{
  *f = (struct og_frame){ 0 };
  if (cls == OG_CLASS_BACKGROUND || p->mode == OG_MODE_GOVERNOR) return OG_ACTION_NONE;
  if (cls != OG_CLASS_RENDER && (!p->last_render_ns || now_ns - p->last_render_ns > (uint64_t)p->follow_us * 1000)) {
    // Not a counted frame. But a background context's lowest level may still be in force (KGSL keeps it until it expires,
    // and only a faster level ends it sooner): this work, which another process may wait for (a widget's drawing at a
    // still screen), would run at the lowest clock. The top ends it, once per lowest episode.
    if (p->mode == OG_MODE_DEADLINE && p->last_bg_min_ns && now_ns - p->last_bg_min_ns < 200000000ull) {
      p->last_bg_min_ns = 0;
      p->told = OG_LEVEL_TOP;
      p->lowest_undone++;
      return OG_ACTION_TOP;
    }
    return OG_ACTION_NONE;
  }
  const uint64_t gap_us = p->last_frame_ns ? (now_ns - p->last_frame_ns) / 1000 : UINT64_MAX;
  const bool cold = gap_us >= p->idle_us;
  if (cls == OG_CLASS_RENDER) {
    if (!cold && p->last_render_ns) {
      const uint64_t since = (now_ns - p->last_render_ns) / 1000;
      if (since >= 500) p->interval_us = og_avg(p->interval_us, since > UINT32_MAX ? UINT32_MAX : (uint32_t)since);   // a second submission of one frame is not an interval
    }
    p->last_render_ns = now_ns;
  }
  p->last_frame_ns = now_ns;
  uint32_t budget = p->budget_us;
  if (deadline_us) budget = deadline_us < 1000 ? 1000 : deadline_us;
  else if (!cold && p->interval_us > budget) budget = p->interval_us < p->slow_budget_us ? p->interval_us : p->slow_budget_us;
  const uint32_t fit = og_fit(p, budget);
  enum og_level want = p->level;
  if (p->mode == OG_MODE_HOLD) want = OG_LEVEL_TOP;                     // 0005's rule: every frame at the top
  else if (cold) {
    // The governor sits at its lowest level after a pause: a frame known to take that long there gets the top.
    const uint32_t predict = p->cold_us ? p->cold_us : p->top_us ? og_at_bottom(p, p->top_us) : 0;
    want = !predict || predict > fit ? OG_LEVEL_TOP : OG_LEVEL_GOVERNOR;
    // Background contexts were at the lowest level just now: KGSL keeps that until it expires unless a faster one comes.
    if (p->last_bg_min_ns && now_ns - p->last_bg_min_ns < 200000000ull) want = OG_LEVEL_TOP;
    p->easy = 0;
    p->down_need = p->down_frames;
  } else if (p->level == OG_LEVEL_GOVERNOR && p->gov_us > fit) want = OG_LEVEL_TOP;
  if (want != p->level) { if (want == OG_LEVEL_TOP) p->ups++; else p->downs++; p->easy = 0; }
  p->level = want;
  p->frames++;
  if (want == OG_LEVEL_TOP) p->frames_top++;
  *f = (struct og_frame){ .level = (uint8_t)want, .cold = cold, .budget_us = budget };
  enum og_action action = want == OG_LEVEL_TOP ? OG_ACTION_TOP : p->told == OG_LEVEL_TOP ? OG_ACTION_GOVERNOR : OG_ACTION_NONE;
  p->told = want;
  return action;
}

// The frame `f` is done: `gpu_us` from its submission (or the previous retirement of its ring, if later) to its KGSL
// timestamp's retirement.
static inline void og_frame_done(struct og_policy *p, const struct og_frame *f, uint32_t gpu_us)
{
  if (!f->budget_us) return;
  const uint32_t fit = og_fit(p, f->budget_us);
  if (gpu_us > f->budget_us) p->late++;
  if (f->level == OG_LEVEL_TOP) {
    p->top_us = og_avg(p->top_us, gpu_us);
    if (p->mode != OG_MODE_DEADLINE) return;
    p->easy = og_at_bottom(p, gpu_us) <= fit ? p->easy + 1 : 0;
    if (p->level == OG_LEVEL_TOP && p->easy >= p->down_need) { p->level = OG_LEVEL_GOVERNOR; p->downs++; p->easy = 0; }
    return;
  }
  p->gov_us = og_avg(p->gov_us, gpu_us);
  if (f->cold) p->cold_us = og_avg(p->cold_us, gpu_us);
  if (gpu_us > fit) {
    // It (nearly) missed at the governor's clock: the next frames at the top, and longer before trying again.
    p->misses++;
    if (p->level != OG_LEVEL_TOP) { p->level = OG_LEVEL_TOP; p->ups++; }
    p->easy = 0;
    p->down_need = p->down_need < 64 ? p->down_need * 4 : 64;
  }
}

// Whether background contexts get the lowest level now: no counted frame for quiet_us (the screen is still).
static inline bool og_background_min(struct og_policy *p, uint64_t now_ns)
{
  if (p->mode != OG_MODE_DEADLINE || (p->last_frame_ns && now_ns - p->last_frame_ns < (uint64_t)p->quiet_us * 1000)) return false;
  p->last_bg_min_ns = now_ns;
  return true;
}

// The guest's Metal runtime waits for a fence in slices of 100 ms (metal2vulkan command_buffer/completion.rs WAIT_SLICE) and
// waits again after each; the tunnel counts each slice that runs out as one of oneios-gpu-report's "fence timeouts". A fence
// that took this long from submission to retirement on the host is one of those (xnuhost's 10 s "fences over 100 ms" line).
#define OG_FENCE_SLOW_US 100000u

// Whether a background context's submission at `now_ns` carries KGSL's lowest level. KGSL's lowest is the whole GPU's (it
// holds until it expires unless a faster level comes), so it never applies while:
//   - another context has GPU work queued (`others_pending`: fences of non-background contexts not yet retired): that work
//     would run at the lowest clock too (a widget renderer drawing at a still screen counts no frame for the level);
//   - another process reads this context's surfaces (`shared`): someone waits for this work;
//   - this context's oldest fence not yet retired was submitted bg_bound_us ago or more (`oldest_ns`, 0: none): aging, the
//     work that waited past the bound runs at the governor's clock, so it is done well inside Metal's 100 ms wait slice.
// Otherwise the screen's stillness decides (og_background_min).
static inline bool og_lowest_for(struct og_policy *p, uint64_t now_ns, uint32_t others_pending, bool shared, uint64_t oldest_ns)
{
  if (p->mode != OG_MODE_DEADLINE) return false;
  if (others_pending || shared || (oldest_ns && now_ns > oldest_ns && now_ns - oldest_ns >= (uint64_t)p->bg_bound_us * 1000)) {
    if (!p->last_frame_ns || now_ns - p->last_frame_ns >= (uint64_t)p->quiet_us * 1000) p->lowest_refused++;
    return false;
  }
  return og_background_min(p, now_ns);
}
#endif

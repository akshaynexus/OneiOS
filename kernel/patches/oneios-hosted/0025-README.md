# 0025-text-fixup-once: user pages that fail their signature or change under a process (Q6)

Owner: fable-user. Date: 2026-11-11. A diff on top of the series up to `0024`. It goes with a change in
xnuhost (`runtime/xnuhost/src/task.c`, same commit), which is where the worst of the three causes was.

## What was wrong

Seen in every boot to launchd with 8 vCPUs: `Text page corruption detected for pid N`, dyld's
`Assertion failed: (!sMemoryManagerInitialized)` in bursts, `xpcproxy` killed by a pointer-authentication or a
guard exception, `thread_create ... failed (-110)`, `thread_run ... failed (-3)`, and xnuhost's
`order N took long: answered after 1 s`.

1. **Two iOS processes in one Linux process** (xnuhost). `xh_task_create` chose a free table entry, then waited
   for the factory's answer with `pthread_cond_timedwait`, which releases the table lock, and marked the entry
   used only afterwards. Two vCPUs creating tasks at the same moment (launchd spawning several daemons) got the
   same entry and the same answer: two kernel address spaces with one handle and one Linux process, the other
   process orphaned. A new process then found another one's memory in place (dyld's allocator already
   initialized), threads ran in the wrong address space, and the exit of one took the process away from the other.
   Fix: one creation at a time.
2. **Waits that were not waits** (xnuhost). `wait_word`, `xh_thread_destroy` and `xh_task_destroy` counted
   `futex`/`usleep` calls instead of time. A vCPU thread gets signals all the time and an interrupted wait
   returns at once, so "10 s" could be 30 ms: `order 4 ... was not answered` and `thread_create` -110 with a
   service thread that was simply not done yet, and a task's slots (trap pages its process has mapped) given to
   the next task while the old process still lived. Fix: by the clock; slots are released when the factory has
   reported the process dead (a thread's slot when Linux cleared its tid).
3. **The text fixup at validation** (kernel, from `0002`). `cs_text_fixup_page` called the host's `text_fixup`
   for every validated page of a signed file:
   - a fixed page no longer has the bytes of its hash. XNU clears a page's validation whenever a UPL over it is
     committed (a `read()` of the file; page 0 of an executable at every exec) and validates it again at the next
     fault: the hash failed, "Text page corruption detected", the process was killed;
   - data and link-edit pages are validated too, and `text_fixup` matches 32-bit patterns: a data word that
     looks like `mrs xN, TPIDR_EL0` or `svc #0x80` was rewritten.
4. `answered after 1 s` was not a stall: the test compared `tv_sec` only (the lead fixed the message; the
   count-based wait next to it is point 2).
5. **Console output with interrupts masked** in `hosted_user_loop` (`panic: Console I/O from interrupt-disabled
   context`, Q4a's boots): mine from `0002`, fixed by the lead's `0023`.
6. **Q4a's "system call 372 returned -1"** in a new workqueue thread of backboardd. 372 is `thread_selfid`, which
   cannot fail in the kernel; -1 is what the host itself returns (EPERM) when it executes a `svc`. So that `svc`
   was not trapped: unmarked text under qemu, or the thread ran in another process's memory (point 1). Not
   reproduced after the fixes; not proven to be either.

## What the patch does

| Where | What |
|---|---|
| `osfmk/arm64/hosted/hosted_text.c` (new) | `hosted_text_fix_page(page, object, offset)`: the host's `text_fixup` on one page, once. Keeps a record per physical page: the page's identity (object, offset) and every changed word, before and after. A page whose record matches and whose words are the "after" ones is left alone. `hosted_text_original()`: a copy of a fixed page with the original words, for hashing |
| `osfmk/vm/vm_fault.c` `vm_fault_enter_prepare` | after `vm_fault_validate_cs` succeeded: a page of a file (not an internal object) that is entered into a user pmap with `VM_PROT_EXECUTE` gets `hosted_text_fix_page`. So only code is fixed, before its first execute mapping in any address space; a private copy of a fixed page already has the fixups |
| `osfmk/vm/vm_fault.c` `vm_page_validate_cs_mapped_slow` | hashes `hosted_text_original()`'s copy when the page carries fixups; a hash that still fails is reported once per case as `hosted cs: FILE offset ... fails its hash in pid N (with/without the host's fixups)` |
| `bsd/kern/ubc_subr.c` `cs_text_fixup_page` | no longer calls `hosted_text_fixup` (Droid-VM's boot-argument fixups there are as they were) |
| `osfmk/arm64/hosted/hosted_user.c` | a twin that cannot be made is tried three more times before the panic; the audit calls below |
| `osfmk/arm64/hosted/hosted_pmap.h`, `arm/pmap/pmap.h`, `pmap.c` | **`hosted_mapcheck=1`** (boot argument, off by default): every `task_map` and `task_unmap` is recorded per pmap (va -> page, protection) and per page (count). Reports, as `hosted check:` lines: a fill at an address that still had another page (an entry changed with no flush between), a page the VM frees (`pmap_recycle_page`) while a task maps it, and `hosted_check_audit()` (every 1024 returns to user mode, and at a task's `brk`): a host mapping the page table does not allow, looked at again after 2 ms before it counts |

## Tests (PC, `qemu-aarch64 -cpu cortex-a710`, 8 vCPUs, the real image set, `kernel.kc` for this kernel)

All with `oneios-data-wait=3000`; the same Data store kept between runs.

| Run | Kernel / xnuhost | Result |
|---|---|---|
| 1 | fixup record + execute-only; xnuhost as committed | no "Text page corruption" (three runs before had it every time); four dyld assertions in one second; `order 4 was not answered after 33 ms` -> `thread_create` -110 -> panic |
| 2, 3 | + waits by the clock, slots kept until the process is dead | no dyld assertion in run 3; one PAC failure, one guard exception; then a twin lost (`thread_run` -3) |
| 5 | + `hosted_mapcheck=1`, the origin series | three dyld assertions in one second, then `thread_run ... failed (-3)`, `thread_create in task 48 failed (-3)`: the task was gone under a live process. That pointed at the handle shared by two address spaces |
| 6 | + `xh_task_create` one at a time | **none of the lines above in 14.5 minutes past "Early boot complete"**: 129 processes, 137 shared-region unnests, backboardd alive 7 minutes after opening the framebuffer; no `hosted check:` line. Ended by me |
| 7 | the same on the series with `0023`/`0024` (this patch as committed) | none of the lines above 7 minutes past "Early boot complete" (139 pids, 168 unnests), no `hosted check:` line; still running when this was written |

`hosted_mapcheck=1` in runs 5 to 7: no fill over another page and no page freed while mapped. The audit of
run 5 (before the 2 ms second look) reported two mappings of launchd that were writable in the host while the page
table had them read-only with the "was writable" bit: a range whose modify bits were being cleared, between the
entries' change and the range's flush. `make -C runtime/xnuhost test`: passes (no test creates tasks from two
threads at once: the race was found by the boots).

## Not done, not known

- Not run on the phone. With seccomp there the `svc` is trapped whatever the text says; the TSD fixups, the task
  table and the waits are the same code.
- Run 3 ended with a thread that did not exit within 10 s and a `clone` order not answered within 10 s, on a PC
  running four agents' boots. xnuhost now logs each Linux thread's state and system call when that happens; it
  did not happen again, so the cause is not known.
- A page that is fixed while xnuhost cannot allocate the record (never seen) runs correctly but fails its next
  validation.
- On real hardware the instruction cache of a page the kernel changes through its window is the host's business:
  Linux flushes when a page is first mapped executable in a process, which is after the fixup; a physical page
  reused for other code relies on the same rule. Not checked on the phone.

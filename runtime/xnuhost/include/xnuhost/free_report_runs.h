/*
 * OneiOS: free-page reporting's long runs (hosted kernel patch 0066, on 0058's free_report.h). Freestanding C, no state.
 *
 * 0058 took up to 64 pages at a time off XNU's free queue, coldest first. Freed pages come back to the free queue in
 * the order iOS frees them, so 64 of them are 64 places scattered over the RAM: on the phone (2026-11-12 23:45 boot)
 * 205 MiB went back in 822 batches as 52602 runs: each run one 4 KiB page, one ram_discard call. 0066
 * takes each cold page with the free, held pages next to it in physical memory (vm_resident.c), in batches of up to
 * 1024 pages when the free queue holds that much above keep, and gives a batch back as its runs: sorted by page
 * number, neighbours joined.
 *
 * The same file is osfmk/arm64/hosted/free_report_runs.h in the kernel (0066 adds it); runtime/xnuhost's host tests
 * check that the two are the same and test these functions (tests/test_xnuhost.c, "free report").
 */
#ifndef XNUHOST_FREE_REPORT_RUNS_H
#define XNUHOST_FREE_REPORT_RUNS_H

#include <stdint.h>
#include <stdbool.h>

/* Sorts `n` page numbers in place, ascending: a heap sort (no recursion, no memory, n log n for a batch of 1024). */
static inline void
xh_fr_sort(uint32_t *pages, uint32_t n)
{
	if (n < 2) {
		return;
	}
	for (uint32_t start = n / 2; start-- > 0;) {        /* the heap: every parent at least its children */
		uint32_t root = start;
		for (uint32_t child; (child = 2 * root + 1) < n; root = child) {
			if (child + 1 < n && pages[child] < pages[child + 1]) {
				child++;
			}
			if (pages[root] >= pages[child]) {
				break;
			}
			uint32_t v = pages[root]; pages[root] = pages[child]; pages[child] = v;
		}
	}
	for (uint32_t end = n - 1; end > 0; end--) {        /* the largest to the end, the heap one shorter */
		uint32_t v = pages[0]; pages[0] = pages[end]; pages[end] = v;
		uint32_t root = 0;
		for (uint32_t child; (child = 2 * root + 1) < end; root = child) {
			if (child + 1 < end && pages[child] < pages[child + 1]) {
				child++;
			}
			if (pages[root] >= pages[child]) {
				break;
			}
			v = pages[root]; pages[root] = pages[child]; pages[child] = v;
		}
	}
}

/*
 * The pages of a batch as ranges for the host: sorts `pages` in place and writes runs of consecutive numbers to
 * first[] and count[] (room for n of each). Returns the number of runs. Duplicates (none should exist) are merged, so
 * a page is never given twice. The same result as free_report.h's xh_fr_runs, which sorts by insertion (fine for 64).
 */
static inline uint32_t
xh_fr_sorted_runs(uint32_t *pages, uint32_t n, uint32_t *first, uint32_t *count)
{
	xh_fr_sort(pages, n);
	uint32_t runs = 0;
	for (uint32_t i = 0; i < n; i++) {
		if (runs > 0 && pages[i] <= first[runs - 1] + count[runs - 1] - 1) {
			continue;                                   /* a duplicate */
		}
		if (runs > 0 && pages[i] == first[runs - 1] + count[runs - 1]) {
			count[runs - 1]++;
			continue;
		}
		first[runs] = pages[i];
		count[runs] = 1;
		runs++;
	}
	return runs;
}

/*
 * How many pages one batch takes: what the free queues hold above keep (`excess`), at most `most`. `slice` is how many
 * the reporter takes, or puts back, under one hold of the free queue's lock (it drops the lock between slices, so an
 * allocation waits for at most one slice); a batch is a whole number of slices where it can be.
 */
static inline uint32_t
xh_fr_batch_pages(uint32_t excess, uint32_t most, uint32_t slice)
{
	if (excess == 0 || most == 0) {
		return 0;
	}
	uint32_t n = excess < most ? excess : most;
	if (slice != 0 && n > slice && n % slice != 0 && n != excess) {
		n -= n % slice;
	}
	return n;
}

/*
 * The pages a cold page brings with it: up to `room` more (what the batch and the free queue's excess still allow),
 * at most `run_most` in one run. `ahead` and `behind` are how many free, held pages follow and precede it in physical
 * memory (the caller counts at most run_most of each). Returns how many of those ahead to take; *take_behind gets
 * the ones behind. Pages ahead first: the coldest page is often the first of what an exit freed.
 */
static inline uint32_t
xh_fr_neighbours(uint32_t ahead, uint32_t behind, uint32_t room, uint32_t run_most, uint32_t *take_behind)
{
	uint32_t most = run_most > 0 ? run_most - 1 : 0;   /* the cold page itself is one of the run */
	if (most > room) {
		most = room;
	}
	uint32_t a = ahead < most ? ahead : most;
	uint32_t b = behind < most - a ? behind : most - a;
	*take_behind = b;
	return a;
}

#endif /* XNUHOST_FREE_REPORT_RUNS_H */

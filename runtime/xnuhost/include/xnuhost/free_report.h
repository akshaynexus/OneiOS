/*
 * OneiOS: free-page reporting, the guest's decisions (hosted kernel patch 0058). Freestanding C, no state.
 *
 * The goal: iOS keeps all of its ram=auto memory; the host gets back only pages iOS has freed. The guest gives
 * back pages that sit on XNU's own free queue (memory iOS is not using at all), never cached, compressed or wired
 * pages, through the ram_discard host call (a hole punched in the RAM memfd: the page reads as zero when it is
 * used again). XNU's free count, targets and memorystatus are not changed by this.
 *
 * The same file is osfmk/arm64/hosted/free_report.h in the kernel (0058 adds it); runtime/xnuhost's host tests
 * check that the two are the same and test these functions (tests/test_xnuhost.c, "free report").
 */
#ifndef XNUHOST_FREE_REPORT_H
#define XNUHOST_FREE_REPORT_H

#include <stdint.h>
#include <stdbool.h>

/*
 * How many pages to give back now. `held` is the number of free-queue pages the host still holds (freed by iOS
 * since they were last given back), `keep` the number the guest always leaves held (the pages the next
 * allocations take: giving them back would only make the host fill them again), `hysteresis` how far above
 * `keep` the count must go before a round starts, `batch` the most pages taken off the free queue at once.
 * `active` is true while a round runs: a round starts above keep + hysteresis and goes on down to keep, so that a
 * free count that moves around keep does not start a round for every few pages.
 */
static inline uint32_t
xh_fr_batch(uint32_t held, uint32_t keep, uint32_t hysteresis, uint32_t batch, bool active)
{
	uint64_t start = (uint64_t)keep + (active ? 0 : hysteresis);
	if ((uint64_t)held <= start || batch == 0) {
		return 0;
	}
	return held - keep < batch ? held - keep : batch;
}

/*
 * The pages of a batch as ranges for the host: sorts `pages` (page numbers) in place and writes runs of
 * consecutive numbers to first[] and count[] (room for n of each). Returns the number of runs. Duplicates (none
 * should exist) are merged, so a page is never given twice.
 */
static inline uint32_t
xh_fr_runs(uint32_t *pages, uint32_t n, uint32_t *first, uint32_t *count)
{
	for (uint32_t i = 1; i < n; i++) {               /* insertion sort: n is a batch (<= 256) */
		uint32_t v = pages[i], j = i;
		while (j > 0 && pages[j - 1] > v) {
			pages[j] = pages[j - 1];
			j--;
		}
		pages[j] = v;
	}
	uint32_t runs = 0;
	for (uint32_t i = 0; i < n; i++) {
		if (runs > 0 && pages[i] <= first[runs - 1] + count[runs - 1]) {
			if (pages[i] == first[runs - 1] + count[runs - 1]) {
				count[runs - 1]++;
			}
			continue;
		}
		first[runs] = pages[i];
		count[runs] = 1;
		runs++;
	}
	return runs;
}

/*
 * The share of `keep` each free queue (XNU's page colours) leaves held: the reporter takes from each queue only
 * what it holds above this, coldest first.
 */
static inline uint32_t
xh_fr_keep_per_queue(uint32_t keep, uint32_t queues)
{
	if (queues == 0) {
		return keep;
	}
	return (keep + queues - 1) / queues;
}

#endif /* XNUHOST_FREE_REPORT_H */

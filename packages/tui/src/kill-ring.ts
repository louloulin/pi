/**
 * Ring buffer for Emacs-style kill/yank operations.
 *
 * Tracks killed (deleted) text entries. Consecutive kills can accumulate
 * into a single entry. Supports yank (paste most recent) and yank-pop
 * (cycle through older entries).
 *
 * The ring stores entries with newest at the end (index = length-1).
 * Yank-pop cycles through entries from newest to oldest.
 */
export class KillRing {
	private ring: string[] = [];

	/**
	 * Add text to the kill ring.
	 *
	 * @param text - The killed text to add
	 * @param opts - Push options
	 * @param opts.prepend - If accumulating, prepend (backward deletion) or append (forward deletion)
	 * @param opts.accumulate - Merge with the most recent entry instead of creating a new one
	 */
	push(text: string, opts: { prepend: boolean; accumulate?: boolean }): void {
		if (!text) return;

		if (opts.accumulate && this.ring.length > 0) {
			const last = this.ring.pop()!;
			this.ring.push(opts.prepend ? text + last : last + text);
		} else {
			this.ring.push(text);
		}
	}

	/**
	 * Get the current entry for yanking.
	 * The newest entry is at index ring.length - 1.
	 */
	peek(): string | undefined {
		return this.ring.length > 0 ? this.ring[this.ring.length - 1] : undefined;
	}

	/**
	 * Move current entry pointer backward (toward older entries) for yank-pop.
	 * After yank-pop, the next peek() should return the previous entry.
	 *
	 * This is achieved by moving the last element (newest) to the front,
	 * so subsequent peek() calls return older entries.
	 */
	rotate(): void {
		if (this.ring.length > 1) {
			// Move the last element (newest) to the front.
			// This makes the second-newest become the last element,
		// so peek() returns the older entry next time.
			const last = this.ring.pop()!;
			this.ring.unshift(last);
		}
	}

	get length(): number {
		return this.ring.length;
	}
}

import { getKeybindings } from "../keybindings.ts";
import { decodeKittyPrintable } from "../keys.ts";
import { KillRing } from "../kill-ring.ts";
import { type Component, CURSOR_MARKER, type Focusable, type TuiMouseEvent, type TuiMouseEventResult } from "../tui.ts";
import { UndoStack } from "../undo-stack.ts";
import { getGraphemeSegmenter, isWhitespaceChar, sliceByColumn, truncateToWidth, visibleWidth } from "../utils.ts";
import { findWordBackward, findWordForward } from "../word-navigation.ts";

const segmenter = getGraphemeSegmenter();

interface InputState {
	value: string;
	cursor: number;
}

/**
 * Visual caret position: (row, column) in display cells.
 * Matches Martty's caret tracking for consistent multi-line behavior.
 */
interface VisualCaret {
	row: number;
	col: number;
}

/**
 * Visual layout result: carets array + row count.
 * Each entry represents the caret position after each character.
 */
interface VisualLayoutResult {
	carets: VisualCaret[];
	rows: number;
}

/**
 * Visual candidate for cursor positioning: index, row, column, and wrap end affinity.
 * Mirrors Martty's visual_candidates() entry.
 */
interface VisualCandidate {
	index: number;
	row: number;
	col: number;
	wrapEnd: boolean;
}

export interface InputOptions {
	prompt?: string;
	placeholder?: string;
	placeholderStyle?: (text: string) => string;
}

/**
 * Jump mode direction - mirrors Rust's JumpDirection enum.
 * When armed, the next printable character jumps to its next/previous occurrence.
 */
export const JUMP_DIRECTION = {
	Forward: "forward",
	Backward: "backward",
} as const;

export type JumpDirection = (typeof JUMP_DIRECTION)[keyof typeof JUMP_DIRECTION];

/**
 * Input component - single-line text input with horizontal scrolling
 */
export class Input implements Component, Focusable {
	private value: string = "";
	private cursor: number = 0; // Cursor position in the value
	private readonly prompt: string;
	private readonly placeholder: string;
	private readonly placeholderStyle: (text: string) => string;
	private renderedStartColumn = 0;

	// Sticky visual column for vertical movement (Martty-style).
	// When moving up/down in multi-line, preserve the original display column.
	private preferredVisualCol: number | null = null;

	// Wrap end affinity - when cursor is at a soft wrap boundary, this tracks
	// whether it should prefer the previous row's end position instead of the next row's start.
	// Mirrors Martty's `cursor_at_wrap_end` field.
	private cursorAtWrapEnd: boolean = false;

	// Last render width - used for vertical movement calculations.
	private lastRenderWidth: number = 80;

	// Cached visual layout for current width.
	private cachedLayout: VisualLayoutResult | null = null;
	private cachedLayoutWidth: number = 0;
	public onSubmit?: (value: string) => void;
	public onEscape?: () => void;

	/** Focusable interface - set by TUI when focus changes */
	focused: boolean = false;

	// Bracketed paste mode buffering
	private pasteBuffer: string = "";
	private isInPaste: boolean = false;

	// Kill ring for Emacs-style kill/yank operations
	private killRing = new KillRing();
	private lastAction: "kill" | "yank" | "type-word" | null = null;

	// Undo support
	private undoStack = new UndoStack<InputState>();

	// Jump mode - mirrors Rust's jump_mode field
	// When armed, the next printable character jumps to its next/previous occurrence.
	private jumpMode: JumpDirection | null = null;

	constructor(options: InputOptions = {}) {
		this.prompt = options.prompt ?? "> ";
		this.placeholder = options.placeholder ?? "";
		this.placeholderStyle = options.placeholderStyle ?? ((text) => text);
	}

	getValue(): string {
		return this.value;
	}

	setValue(value: string): void {
		this.value = value;
		this.cursor = Math.min(this.cursor, value.length);
		// Invalidate visual layout cache when value changes
		this.cachedLayout = null;
	}

	handleInput(data: string): void {
		// Handle bracketed paste mode
		// Start of paste: \x1b[200~
		// End of paste: \x1b[201~

		// Check if we're starting a bracketed paste
		if (data.includes("\x1b[200~")) {
			this.isInPaste = true;
			this.pasteBuffer = "";
			data = data.replace("\x1b[200~", "");
		}

		// If we're in a paste, buffer the data
		if (this.isInPaste) {
			// Check if this chunk contains the end marker
			this.pasteBuffer += data;

			const endIndex = this.pasteBuffer.indexOf("\x1b[201~");
			if (endIndex !== -1) {
				// Extract the pasted content
				const pasteContent = this.pasteBuffer.substring(0, endIndex);

				// Process the complete paste
				this.handlePaste(pasteContent);

				// Reset paste state
				this.isInPaste = false;

				// Handle any remaining input after the paste marker
				const remaining = this.pasteBuffer.substring(endIndex + 6); // 6 = length of \x1b[201~
				this.pasteBuffer = "";
				if (remaining) {
					this.handleInput(remaining);
				}
			}
			return;
		}

		const kb = getKeybindings();

		// Escape/Cancel
		if (kb.matches(data, "tui.select.cancel")) {
			if (this.onEscape) this.onEscape();
			return;
		}

		// Undo
		if (kb.matches(data, "tui.editor.undo")) {
			this.undo();
			return;
		}

		// Submit
		if (kb.matches(data, "tui.input.submit") || data === "\n") {
			if (this.onSubmit) this.onSubmit(this.value);
			return;
		}

		// Deletion
		if (kb.matches(data, "tui.editor.deleteCharBackward")) {
			this.handleBackspace();
			return;
		}

		if (kb.matches(data, "tui.editor.deleteCharForward")) {
			this.handleForwardDelete();
			return;
		}

		if (kb.matches(data, "tui.editor.deleteWordBackward")) {
			this.deleteWordBackwards();
			return;
		}

		if (kb.matches(data, "tui.editor.deleteWordForward")) {
			this.deleteWordForward();
			return;
		}

		if (kb.matches(data, "tui.editor.deleteToLineStart")) {
			this.deleteToLineStart();
			return;
		}

		if (kb.matches(data, "tui.editor.deleteToLineEnd")) {
			this.deleteToLineEnd();
			return;
		}

		// Kill ring actions
		if (kb.matches(data, "tui.editor.yank")) {
			this.yank();
			return;
		}
		if (kb.matches(data, "tui.editor.yankPop")) {
			this.yankPop();
			return;
		}

		// Cursor movement
		if (kb.matches(data, "tui.editor.cursorLeft")) {
			this.lastAction = null;
			if (this.cursor > 0) {
				const beforeCursor = this.value.slice(0, this.cursor);
				const graphemes = [...segmenter.segment(beforeCursor)];
				const lastGrapheme = graphemes[graphemes.length - 1];
				this.cursor -= lastGrapheme ? lastGrapheme.segment.length : 1;
			}
			// Reset preferred column on explicit cursor movement
			this.preferredVisualCol = null;
			return;
		}

		if (kb.matches(data, "tui.editor.cursorRight")) {
			this.lastAction = null;
			if (this.cursor < this.value.length) {
				const afterCursor = this.value.slice(this.cursor);
				const graphemes = [...segmenter.segment(afterCursor)];
				const firstGrapheme = graphemes[0];
				this.cursor += firstGrapheme ? firstGrapheme.segment.length : 1;
			}
			// Reset preferred column on explicit cursor movement
			this.preferredVisualCol = null;
			return;
		}

		if (kb.matches(data, "tui.editor.cursorLineStart")) {
			this.lastAction = null;
			this.moveToVisualLineStart(this.lastRenderWidth);
			return;
		}

		if (kb.matches(data, "tui.editor.cursorLineEnd")) {
			this.lastAction = null;
			this.moveToVisualLineEnd(this.lastRenderWidth);
			return;
		}

		// Vertical movement - mirrors Martty's move_vertical()
		if (kb.matches(data, "tui.editor.cursorUp")) {
			this.lastAction = null;
			this.moveVertical(this.lastRenderWidth, -1);
			return;
		}

		if (kb.matches(data, "tui.editor.cursorDown")) {
			this.lastAction = null;
			this.moveVertical(this.lastRenderWidth, 1);
			return;
		}

		if (kb.matches(data, "tui.editor.cursorWordLeft")) {
			this.moveWordBackwards();
			return;
		}

		if (kb.matches(data, "tui.editor.cursorWordRight")) {
			this.moveWordForwards();
			return;
		}

		// Jump mode controls - arm jump mode for forward/backward search
		if (kb.matches(data, "tui.editor.jumpForward")) {
			this.armJumpMode(JUMP_DIRECTION.Forward);
			return;
		}

		if (kb.matches(data, "tui.editor.jumpBackward")) {
			this.armJumpMode(JUMP_DIRECTION.Backward);
			return;
		}

		// Kitty CSI-u printable character (e.g. \x1b[97u for 'a').
		// Terminals with Kitty protocol flag 1 (disambiguate) send CSI-u for all keys,
		// including plain printable characters. Decode before the control-char check
		// since CSI-u sequences contain \x1b which would be rejected.
		const kittyPrintable = decodeKittyPrintable(data);
		if (kittyPrintable !== undefined) {
			// Jump mode: if armed, jump to the next/previous occurrence of this character
			if (this.jumpMode !== null) {
				const jumped = this.jumpToChar(kittyPrintable, this.jumpMode);
				this.jumpMode = null; // Consume the jump
				if (jumped) return;
			}
			this.insertCharacter(kittyPrintable);
			return;
		}

		// Regular character input - accept printable characters including Unicode,
		// but reject control characters (C0: 0x00-0x1F, DEL: 0x7F, C1: 0x80-0x9F)
		const hasControlChars = [...data].some((ch) => {
			const code = ch.charCodeAt(0);
			return code < 32 || code === 0x7f || (code >= 0x80 && code <= 0x9f);
		});
		if (!hasControlChars) {
			// Jump mode: if armed, jump to the next/previous occurrence of this character
			if (this.jumpMode !== null) {
				const jumped = this.jumpToChar(data, this.jumpMode);
				this.jumpMode = null; // Consume the jump
				if (jumped) return;
			}
			this.insertCharacter(data);
		}
	}

	handleMouse(event: TuiMouseEvent): TuiMouseEventResult | undefined {
		if (event.type !== "press" || event.button !== "left" || event.y !== 0) return undefined;
		const visibleColumn = Math.max(0, event.x - 2);
		const targetColumn = this.renderedStartColumn + visibleColumn;
		let currentColumn = 0;
		this.cursor = this.value.length;
		for (const grapheme of segmenter.segment(this.value)) {
			const nextColumn = currentColumn + visibleWidth(grapheme.segment);
			if (targetColumn < nextColumn) {
				this.cursor = grapheme.index;
				break;
			}
			currentColumn = nextColumn;
		}
		this.lastAction = null;
		// Reset preferred column on mouse click positioning
		this.preferredVisualCol = null;
		this.cursorAtWrapEnd = false;
		return { handled: true, focus: true };
	}

	private insertCharacter(char: string): void {
		// For word-boundary undo coalescing, we need to push undo state BEFORE
		// inserting whitespace. This ensures the undo state includes the word.
		const isWhitespace = isWhitespaceChar(char);
		if (isWhitespace) {
			this.pushUndo();
		}

		// Undo coalescing: consecutive word chars coalesce into one undo unit
		// A new word starts if: previous action wasn't "type-word"
		if (!isWhitespace && this.lastAction !== "type-word") {
			this.pushUndo();
		}
		this.lastAction = "type-word";

		this.value = this.value.slice(0, this.cursor) + char + this.value.slice(this.cursor);
		this.cursor += char.length;

		// Invalidate visual layout cache
		this.cachedLayout = null;
		// Reset preferred column on edit
		this.preferredVisualCol = null;
		this.cursorAtWrapEnd = false;
	}

	private handleBackspace(): void {
		this.lastAction = null;
		if (this.cursor > 0) {
			this.pushUndo();
			const beforeCursor = this.value.slice(0, this.cursor);
			const graphemes = [...segmenter.segment(beforeCursor)];
			const lastGrapheme = graphemes[graphemes.length - 1];
			const graphemeLength = lastGrapheme ? lastGrapheme.segment.length : 1;
			this.value = this.value.slice(0, this.cursor - graphemeLength) + this.value.slice(this.cursor);
			this.cursor -= graphemeLength;
			this.cachedLayout = null;
			this.preferredVisualCol = null;
			this.cursorAtWrapEnd = false;
		}
	}

	private handleForwardDelete(): void {
		this.lastAction = null;
		if (this.cursor < this.value.length) {
			this.pushUndo();
			const afterCursor = this.value.slice(this.cursor);
			const graphemes = [...segmenter.segment(afterCursor)];
			const firstGrapheme = graphemes[0];
			const graphemeLength = firstGrapheme ? firstGrapheme.segment.length : 1;
			this.value = this.value.slice(0, this.cursor) + this.value.slice(this.cursor + graphemeLength);
			this.cachedLayout = null;
			this.preferredVisualCol = null;
			this.cursorAtWrapEnd = false;
		}
	}

	private deleteToLineStart(): void {
		if (this.cursor === 0) return;
		this.pushUndo();
		const deletedText = this.value.slice(0, this.cursor);
		this.killRing.push(deletedText, { prepend: true, accumulate: this.lastAction === "kill" });
		this.lastAction = "kill";
		this.value = this.value.slice(this.cursor);
		this.cursor = 0;
	}

	private deleteToLineEnd(): void {
		if (this.cursor >= this.value.length) return;
		this.pushUndo();
		const deletedText = this.value.slice(this.cursor);
		this.killRing.push(deletedText, { prepend: false, accumulate: this.lastAction === "kill" });
		this.lastAction = "kill";
		this.value = this.value.slice(0, this.cursor);
		this.preferredVisualCol = null;
		this.cursorAtWrapEnd = false;
	}

	private deleteWordBackwards(): void {
		if (this.cursor === 0) return;

		// Save lastAction before cursor movement (moveWordBackwards resets it)
		const wasKill = this.lastAction === "kill";

		this.pushUndo();

		const oldCursor = this.cursor;
		this.moveWordBackwards();
		const deleteFrom = this.cursor;
		this.cursor = oldCursor;

		const deletedText = this.value.slice(deleteFrom, this.cursor);
		this.killRing.push(deletedText, { prepend: true, accumulate: wasKill });
		this.lastAction = "kill";

		this.value = this.value.slice(0, deleteFrom) + this.value.slice(this.cursor);
		this.cursor = deleteFrom;
		this.preferredVisualCol = null;
		this.cursorAtWrapEnd = false;
	}

	private deleteWordForward(): void {
		if (this.cursor >= this.value.length) return;

		// Save lastAction before cursor movement (moveWordForwards resets it)
		const wasKill = this.lastAction === "kill";

		this.pushUndo();

		const oldCursor = this.cursor;
		this.moveWordForwards();
		const deleteTo = this.cursor;
		this.cursor = oldCursor;

		const deletedText = this.value.slice(this.cursor, deleteTo);
		this.killRing.push(deletedText, { prepend: false, accumulate: wasKill });
		this.lastAction = "kill";

		this.value = this.value.slice(0, this.cursor) + this.value.slice(deleteTo);
		this.preferredVisualCol = null;
		this.cursorAtWrapEnd = false;
	}

	private yank(): void {
		const text = this.killRing.peek();
		if (!text) return;

		this.pushUndo();

		this.value = this.value.slice(0, this.cursor) + text + this.value.slice(this.cursor);
		this.cursor += text.length;
		this.lastAction = "yank";
		this.preferredVisualCol = null;
		this.cursorAtWrapEnd = false;
	}

	private yankPop(): void {
		if (this.lastAction !== "yank" || this.killRing.length <= 1) return;

		this.pushUndo();

		// Delete the previously yanked text (still at end of ring before rotation)
		const prevText = this.killRing.peek() || "";
		this.value = this.value.slice(0, this.cursor - prevText.length) + this.value.slice(this.cursor);
		this.cursor -= prevText.length;

		// Rotate and insert new entry
		this.killRing.rotate();
		const text = this.killRing.peek() || "";
		this.value = this.value.slice(0, this.cursor) + text + this.value.slice(this.cursor);
		this.cursor += text.length;
		this.lastAction = "yank";
		this.preferredVisualCol = null;
		this.cursorAtWrapEnd = false;
	}

	private pushUndo(): void {
		this.undoStack.push({ value: this.value, cursor: this.cursor });
	}

	private undo(): void {
		const snapshot = this.undoStack.pop();
		if (!snapshot) return;
		this.value = snapshot.value;
		this.cursor = snapshot.cursor;
		this.lastAction = null;
	}

	private moveWordBackwards(): void {
		if (this.cursor === 0) return;
		this.lastAction = null;
		this.cursor = findWordBackward(this.value, this.cursor);
		// Reset preferred column on explicit cursor movement
		this.preferredVisualCol = null;
	}

	private moveWordForwards(): void {
		if (this.cursor >= this.value.length) return;
		this.lastAction = null;
		this.cursor = findWordForward(this.value, this.cursor);
		// Reset preferred column on explicit cursor movement
		this.preferredVisualCol = null;
	}

	/**
	 * Move by one visual row while preserving the original display column.
	 * Mirrors Martty's `move_vertical()` method.
	 * @param width - The terminal width for calculating visual layout
	 * @param direction - -1 for up, +1 for down
	 */
	private moveVertical(width: number, direction: number): void {
		const effectiveWidth = Math.max(1, width);
		const { candidates, rows } = this.computeVisualCandidates(effectiveWidth);
		const { row, col } = this.getVisualCursor(effectiveWidth);

		// Set preferred column on first vertical move
		const goal = this.preferredVisualCol ?? col;
		this.preferredVisualCol = goal;

		// Calculate target row
		let targetRow: number | null;
		if (direction < 0) {
			targetRow = row > 0 ? row - 1 : null;
		} else if (direction > 0 && row + 1 < rows) {
			targetRow = row + 1;
		} else {
			targetRow = null;
		}

		if (targetRow === null) {
			return;
		}

		// Find the best candidate on the target row (closest column to goal)
		let best: { index: number; distance: number; wrapEnd: boolean } | null = null;
		for (const candidate of candidates) {
			if (candidate.row !== targetRow) {
				continue;
			}
			const distance = Math.abs(candidate.col - goal);
			if (best === null || distance < best.distance) {
				best = { index: candidate.index, distance, wrapEnd: candidate.wrapEnd };
			}
		}

		if (best !== null) {
			this.cursor = best.index;
			this.cursorAtWrapEnd = best.wrapEnd;
		}
	}

	/**
	 * Move to the beginning of the current rendered row, not the beginning
	 * of the whole text. Mirrors Martty's `move_to_visual_line_start()` method.
	 * @param width - The terminal width for calculating visual layout
	 */
	private moveToVisualLineStart(width: number): void {
		const effectiveWidth = Math.max(1, width);
		const { candidates } = this.computeVisualCandidates(effectiveWidth);
		const currentRow = this.getVisualCursor(effectiveWidth).row;

		// Find the leftmost position on the current row
		let bestIndex = 0;
		let bestCol = Infinity;
		for (const candidate of candidates) {
			if (candidate.row === currentRow && candidate.col < bestCol) {
				bestCol = candidate.col;
				bestIndex = candidate.index;
			}
		}

		this.cursor = bestIndex;
		this.cursorAtWrapEnd = false;
		this.preferredVisualCol = null;
	}

	/**
	 * Move to the end of the current rendered row. A soft-wrap boundary has
	 * two visual affinities; retain the upstream one so the cursor remains
	 * visibly at this row's end instead of appearing on the next row.
	 * Mirrors Martty's `move_to_visual_line_end()` method.
	 * @param width - The terminal width for calculating visual layout
	 */
	private moveToVisualLineEnd(width: number): void {
		const effectiveWidth = Math.max(1, width);
		const { candidates } = this.computeVisualCandidates(effectiveWidth);
		const currentRow = this.getVisualCursor(effectiveWidth).row;

		// Find the rightmost position on the current row
		let bestIndex = 0;
		let bestCol = -1;
		let bestWrapEnd = false;
		for (const candidate of candidates) {
			if (candidate.row === currentRow && candidate.col > bestCol) {
				bestCol = candidate.col;
				bestIndex = candidate.index;
				bestWrapEnd = candidate.wrapEnd;
			}
		}

		this.cursor = bestIndex;
		this.cursorAtWrapEnd = bestWrapEnd;
		this.preferredVisualCol = null;
	}

	/**
	 * Compute visual candidates with wrap end affinity.
	 * Mirrors Martty's `visual_candidates()` method.
	 */
	private computeVisualCandidates(width: number): { candidates: VisualCandidate[]; rows: number } {
		const layout = this.computeVisualLayout(width);
		const chars: string[] = [...this.value];
		const candidates: VisualCandidate[] = [];

		for (let index = 0; index < layout.carets.length; index++) {
			const caret = layout.carets[index]!;
			candidates.push({
				index,
				row: caret.row,
				col: caret.col,
				wrapEnd: false,
			});

			// Check for wrap end affinity
			const wrapEndPos = this.getWrapEndPosition(index, chars, layout.carets);
			if (wrapEndPos !== null) {
				candidates.push({
					index,
					row: wrapEndPos.row,
					col: wrapEndPos.col,
					wrapEnd: true,
				});
			}
		}

		return { candidates, rows: layout.rows };
	}

	/**
	 * Get the wrap end position for a character boundary.
	 * Returns the position just after the last character of the previous row
	 * (visual row end affinity). Mirrors Martty's `wrap_end_position()` method.
	 */
	private getWrapEndPosition(
		index: number,
		chars: string[],
		carets: VisualCaret[],
	): { row: number; col: number } | null {
		if (index === 0) {
			return null;
		}

		const previous = chars[index - 1];
		if (previous === undefined || previous === "\n") {
			return null;
		}

		const currentCaret = carets[index];
		const prevCaret = carets[index - 1];
		if (currentCaret.row > prevCaret.row) {
			// This is a wrap boundary
			const charWidth = visibleWidth(previous);
			return {
				row: prevCaret.row,
				col: prevCaret.col + Math.max(charWidth, 1),
			};
		}

		return null;
	}

	/**
	 * Arm jump mode - the next printable character will jump to its next/previous occurrence.
	 * Mirrors Rust's `Ctrl+]` / `Ctrl+Alt+]` behavior.
	 * @param direction - Forward (Ctrl+]) or Backward (Ctrl+Alt+])
	 */
	private armJumpMode(direction: JumpDirection): void {
		this.jumpMode = direction;
		// Jump breaks the kill/yank/typing chains
		this.lastAction = null;
	}

	/**
	 * Jump to the next or previous occurrence of a character.
	 * Mirrors Rust's `jump_to_char` method.
	 * @param needle - The character to search for
	 * @param direction - Forward or backward search
	 * @returns true if the cursor moved, false if no match found
	 */
	private jumpToChar(needle: string, direction: JumpDirection): boolean {
		// Jump breaks the kill/yank/typing chains
		this.lastAction = null;

		if (this.value.length === 0 || needle.length === 0) {
			return false;
		}

		const char = needle[0]!;

		if (direction === JUMP_DIRECTION.Forward) {
			// Search forward from cursor position
			// The character under the cursor is never a match (like Rust implementation)
			const searchStart = this.cursor + 1;
			const matchIndex = this.value.indexOf(char, searchStart);
			if (matchIndex !== -1) {
				this.cursor = matchIndex;
				// Reset preferred column on explicit cursor movement
				this.preferredVisualCol = null;
				return true;
			}
		} else {
			// Search backward from cursor position
			// The character under the cursor is never a match (like Rust implementation)
			// Find the last occurrence before cursor
			let matchIndex = -1;
			let pos = this.cursor;
			while (pos > 0) {
				pos--;
				if (this.value[pos] === char) {
					matchIndex = pos;
					break;
				}
			}
			if (matchIndex !== -1) {
				this.cursor = matchIndex;
				// Reset preferred column on explicit cursor movement
				this.preferredVisualCol = null;
				return true;
			}
		}

		return false;
	}

	private handlePaste(pastedText: string): void {
		this.lastAction = null;
		this.pushUndo();

		// Clean the pasted text - remove newlines and carriage returns
		const cleanText = pastedText.replace(/\r\n/g, "").replace(/\r/g, "").replace(/\n/g, "").replace(/\t/g, "    ");

		// Insert at cursor position
		this.value = this.value.slice(0, this.cursor) + cleanText + this.value.slice(this.cursor);
		this.cursor += cleanText.length;

		// Invalidate visual layout cache
		this.cachedLayout = null;
		this.preferredVisualCol = null;
		this.cursorAtWrapEnd = false;
	}

	/**
	 * Compute visual layout for the current text at the given width.
	 * Returns caret positions for each character boundary and total row count.
	 * Mirrors Martty's `visual_layout()` method for consistent behavior.
	 */
	private computeVisualLayout(width: number): VisualLayoutResult {
		// Return cached result if width matches
		if (this.cachedLayout && this.cachedLayoutWidth === width) {
			return this.cachedLayout;
		}

		const chars: string[] = [...this.value];
		const carets: VisualCaret[] = new Array(chars.length + 1);

		let row = 0;
		let col = 0;

		for (let index = 0; index < chars.length; index++) {
			const ch = chars[index]!;
			const charWidth = visibleWidth(ch);

			// Wrap if needed (single-line input wraps at width boundary)
			if (col + charWidth > width) {
				row++;
				col = 0;
			}

			carets[index] = { row, col };
			col += charWidth;
		}

		// Final caret at end of text
		carets[chars.length] = { row, col };

		const result: VisualLayoutResult = {
			carets,
			rows: row + 1,
		};

		// Cache the result
		this.cachedLayout = result;
		this.cachedLayoutWidth = width;

		return result;
	}

	/**
	 * Get the current visual cursor position as (row, column).
	 * Respects wrap end affinity, mirroring Martty's `visual_cursor()` method.
	 */
	getVisualCursor(width: number): VisualCaret {
		const layout = this.computeVisualLayout(width);
		const index = Math.min(this.cursor, layout.carets.length - 1);

		// Check wrap end affinity
		if (this.cursorAtWrapEnd) {
			const chars: string[] = [...this.value];
			const wrapEndPos = this.getWrapEndPosition(index, chars, layout.carets);
			if (wrapEndPos !== null) {
				return wrapEndPos;
			}
		}

		return layout.carets[index]!;
	}

	/**
	 * Get the visual row count at the given width.
	 * Mirrors Martty's `visual_row_count()` method.
	 */
	getVisualRowCount(width: number): number {
		return this.computeVisualLayout(Math.max(1, width)).rows;
	}

	/**
	 * Reset the preferred visual column.
	 * Called when the user explicitly moves to a new position.
	 */
	resetPreferredVisualCol(): void {
		this.preferredVisualCol = null;
		this.cursorAtWrapEnd = false;
	}

	/**
	 * Reset the vertical goal (preferred column and wrap end affinity).
	 * Mirrors Martty's `reset_vertical_goal()` method.
	 */
	resetVerticalGoal(): void {
		this.preferredVisualCol = null;
		this.cursorAtWrapEnd = false;
	}

	invalidate(): void {
		// No cached state to invalidate currently
	}

	render(width: number): string[] {
		// Store the render width for vertical movement calculations
		this.lastRenderWidth = width;

		// Calculate visible window
		const availableWidth = width - visibleWidth(this.prompt);

		if (availableWidth <= 0) {
			return [truncateToWidth(this.prompt, width, "")];
		}

		if (this.value.length === 0 && this.placeholder) {
			const placeholder = truncateToWidth(this.placeholder, availableWidth, "");
			const graphemes = [...segmenter.segment(placeholder)];
			const atCursor = graphemes[0]?.segment ?? " ";
			const afterCursor = placeholder.slice(atCursor.length);
			const marker = this.focused ? CURSOR_MARKER : "";
			const cursorChar = `\x1b[7m${this.placeholderStyle(atCursor)}\x1b[27m`;
			const textWithCursor = marker + cursorChar + this.placeholderStyle(afterCursor);
			const padding = " ".repeat(Math.max(0, availableWidth - visibleWidth(textWithCursor)));
			return [this.prompt + textWithCursor + padding];
		}

		let visibleText = "";
		let cursorDisplay = this.cursor;
		this.renderedStartColumn = 0;
		const totalWidth = visibleWidth(this.value);

		if (totalWidth < availableWidth) {
			// Everything fits (leave room for cursor at end)
			visibleText = this.value;
		} else {
			// Need horizontal scrolling
			// Reserve one column for cursor if it's at the end
			const scrollWidth = this.cursor === this.value.length ? availableWidth - 1 : availableWidth;

			// Use preferred visual column if set, otherwise use current cursor position
			const targetCol = this.preferredVisualCol ?? visibleWidth(this.value.slice(0, this.cursor));

			if (scrollWidth > 0) {
				const halfWidth = Math.floor(scrollWidth / 2);
				let startCol = 0;

				if (targetCol < halfWidth) {
					// Cursor near start
					startCol = 0;
				} else if (targetCol > totalWidth - halfWidth) {
					// Cursor near end
					startCol = Math.max(0, totalWidth - scrollWidth);
				} else {
					// Cursor in middle - center on the preferred/target column
					startCol = Math.max(0, targetCol - halfWidth);
				}

				this.renderedStartColumn = startCol;
				visibleText = sliceByColumn(this.value, startCol, scrollWidth, true);
				const beforeCursor = sliceByColumn(this.value, startCol, Math.max(0, targetCol - startCol), true);
				cursorDisplay = beforeCursor.length;
			} else {
				visibleText = "";
				cursorDisplay = 0;
			}
		}

		// Build line with fake cursor
		// Insert cursor character at cursor position
		const graphemes = [...segmenter.segment(visibleText.slice(cursorDisplay))];
		const cursorGrapheme = graphemes[0];

		const beforeCursor = visibleText.slice(0, cursorDisplay);
		const atCursor = cursorGrapheme?.segment ?? " "; // Character at cursor, or space if at end
		const afterCursor = visibleText.slice(cursorDisplay + atCursor.length);

		// Hardware cursor marker (zero-width, emitted before fake cursor for IME positioning)
		const marker = this.focused ? CURSOR_MARKER : "";

		// Use inverse video to show cursor
		const cursorChar = `\x1b[7m${atCursor}\x1b[27m`; // ESC[7m = reverse video, ESC[27m = normal
		const textWithCursor = beforeCursor + marker + cursorChar + afterCursor;

		// Calculate visual width
		const visualLength = visibleWidth(textWithCursor);
		const padding = " ".repeat(Math.max(0, availableWidth - visualLength));
		const line = this.prompt + textWithCursor + padding;

		return [line];
	}
}

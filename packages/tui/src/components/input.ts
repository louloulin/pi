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

	// Cursor affinity at soft-wrap line end (Martty-style).
	// When true, the cursor visually stays at the end of the previous row
	// rather than at the start of the next row.
	private cursorAtWrapEnd: boolean = false;

	// Cached visual layout for current width.
	private cachedLayoutWidth: number = 0;
	private cachedLayout: VisualLayoutResult | null = null;
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
			this.cursor = 0;
			// Reset preferred column
			this.preferredVisualCol = null;
			return;
		}

		if (kb.matches(data, "tui.editor.cursorLineEnd")) {
			this.lastAction = null;
			this.cursor = this.value.length;
			// Reset preferred column
			this.preferredVisualCol = null;
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
		return { handled: true, focus: true };
	}

	private insertCharacter(char: string): void {
		// Undo coalescing: consecutive word chars coalesce into one undo unit
		if (isWhitespaceChar(char) || this.lastAction !== "type-word") {
			this.pushUndo();
		}
		this.lastAction = "type-word";

		this.value = this.value.slice(0, this.cursor) + char + this.value.slice(this.cursor);
		this.cursor += char.length;

		// Invalidate visual layout cache
		this.cachedLayout = null;
		// Reset preferred column on edit
		this.preferredVisualCol = null;
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
	}

	private yank(): void {
		const text = this.killRing.peek();
		if (!text) return;

		this.pushUndo();

		this.value = this.value.slice(0, this.cursor) + text + this.value.slice(this.cursor);
		this.cursor += text.length;
		this.lastAction = "yank";
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
	 */
	getVisualCursor(width: number): VisualCaret {
		const layout = this.computeVisualLayout(width);
		const index = Math.min(this.cursor, layout.carets.length - 1);
		return layout.carets[index]!;
	}

	/**
	 * Get the total visual row count at the given width.
	 */
	getVisualRowCount(width: number): number {
		return this.computeVisualLayout(width).rows;
	}

	/**
	 * Reset the preferred visual column.
	 * Called when the user explicitly moves to a new position.
	 */
	resetPreferredVisualCol(): void {
		this.preferredVisualCol = null;
	}

	/**
	 * Forget the sticky display column used by repeated vertical motions.
	 * Mirrors Martty's `reset_vertical_goal()` method.
	 */
	resetVerticalGoal(): void {
		this.preferredVisualCol = null;
		this.cursorAtWrapEnd = false;
	}

	/**
	 * All visual cursor position candidates for character boundaries.
	 * Soft wraps contribute both the previous row's end and the next row's start.
	 * Returns tuples of (charIndex, row, col, isWrapEnd).
	 * Mirrors Martty's `visual_candidates()` method.
	 */
	private getVisualCandidates(
		width: number,
	): Array<{ charIndex: number; row: number; col: number; isWrapEnd: boolean }> {
		const layout = this.computeVisualLayout(width);
		const chars: string[] = [...this.value];
		const candidates: Array<{ charIndex: number; row: number; col: number; isWrapEnd: boolean }> = [];

		for (let index = 0; index < layout.carets.length; index++) {
			const { row, col } = layout.carets[index]!;
			candidates.push({ charIndex: index, row, col, isWrapEnd: false });

			// Check for wrap-end affinity at soft-wrap boundaries
			if (index > 0 && index <= chars.length) {
				const wrapEnd = this.getWrapEndPosition(index, chars, layout.carets);
				if (wrapEnd) {
					candidates.push({ charIndex: index, row: wrapEnd.row, col: wrapEnd.col, isWrapEnd: true });
				}
			}
		}

		return candidates;
	}

	/**
	 * Get the wrap-end position for a character boundary.
	 * This is the visual position just after the previous character at a soft-wrap boundary.
	 * Mirrors Martty's `wrap_end_position()` method.
	 */
	private getWrapEndPosition(
		index: number,
		chars: string[],
		carets: VisualCaret[],
	): { row: number; col: number } | null {
		if (index === 0) return null;
		const previousChar = chars[index - 1];
		if (!previousChar || previousChar === "\n") return null;

		const prevCaret = carets[index - 1];
		const currentCaret = carets[index];

		// Only provide wrap-end position if we're at a soft-wrap boundary
		// (current row is different from previous row)
		if (currentCaret.row > prevCaret.row) {
			const charWidth = visibleWidth(previousChar);
			return { row: prevCaret.row, col: prevCaret.col + charWidth };
		}
		return null;
	}

	/**
	 * Get the current visual cursor position, respecting wrap-end affinity.
	 * Mirrors Martty's `visual_cursor()` method.
	 */
	getVisualCursorPosition(width: number): VisualCaret {
		const layout = this.computeVisualLayout(width);
		const index = Math.min(this.cursor, layout.carets.length - 1);

		// If cursor has wrap-end affinity and there's a valid wrap-end position
		if (this.cursorAtWrapEnd) {
			const chars: string[] = [...this.value];
			const wrapEnd = this.getWrapEndPosition(index, chars, layout.carets);
			if (wrapEnd) {
				return { row: wrapEnd.row, col: wrapEnd.col };
			}
		}

		return layout.carets[index]!;
	}

	/**
	 * Move cursor by one visual row while preserving the original display column.
	 * Mirrors Martty's `move_vertical()` method.
	 * @param width - The available display width
	 * @param direction - -1 for up, +1 for down
	 */
	moveVertical(width: number, direction: number): void {
		width = Math.max(1, width);
		const candidates = this.getVisualCandidates(width);
		const layout = this.computeVisualLayout(width);

		if (candidates.length === 0) return;

		const currentPos = this.getVisualCursorPosition(width);
		const goal = this.preferredVisualCol ?? currentPos.col;

		// Determine target row
		let targetRow: number | null = null;
		if (direction < 0) {
			if (currentPos.row > 0) {
				targetRow = currentPos.row - 1;
			}
		} else if (direction > 0) {
			if (currentPos.row + 1 < layout.rows) {
				targetRow = currentPos.row + 1;
			}
		}

		if (targetRow === null) return;

		// Find the best candidate on the target row
		let best: { charIndex: number; distance: number; isWrapEnd: boolean } | null = null;
		for (const candidate of candidates) {
			if (candidate.row !== targetRow) continue;

			const distance = Math.abs(candidate.col - goal);
			if (!best || distance < best.distance) {
				best = { charIndex: candidate.charIndex, distance, isWrapEnd: candidate.isWrapEnd };
			}
		}

		if (best) {
			this.cursor = best.charIndex;
			this.cursorAtWrapEnd = best.isWrapEnd;
			// Set preferred column on first vertical move
			if (this.preferredVisualCol === null) {
				this.preferredVisualCol = goal;
			}
		}
	}

	/**
	 * Move cursor to the beginning of the current rendered row.
	 * Mirrors Martty's `move_to_visual_line_start()` method.
	 */
	moveToVisualLineStart(width: number): void {
		width = Math.max(1, width);
		const candidates = this.getVisualCandidates(width);
		const currentPos = this.getVisualCursorPosition(width);

		// Find the leftmost candidate on the current row
		let best: { charIndex: number; col: number; isWrapEnd: boolean } | null = null;
		for (const candidate of candidates) {
			if (candidate.row !== currentPos.row) continue;
			if (!best || candidate.col < best.col) {
				best = { charIndex: candidate.charIndex, col: candidate.col, isWrapEnd: candidate.isWrapEnd };
			}
		}

		if (best) {
			this.cursor = best.charIndex;
			this.cursorAtWrapEnd = best.isWrapEnd;
			this.preferredVisualCol = null;
		}
	}

	/**
	 * Move cursor to the end of the current rendered row.
	 * A soft-wrap boundary has two visual affinities; this moves to the
	 * wrap-end position so the cursor remains at this row's end.
	 * Mirrors Martty's `move_to_visual_line_end()` method.
	 */
	moveToVisualLineEnd(width: number): void {
		width = Math.max(1, width);
		const candidates = this.getVisualCandidates(width);
		const currentPos = this.getVisualCursorPosition(width);

		// Find the rightmost candidate on the current row
		let best: { charIndex: number; col: number; isWrapEnd: boolean } | null = null;
		for (const candidate of candidates) {
			if (candidate.row !== currentPos.row) continue;
			if (!best || candidate.col > best.col) {
				best = { charIndex: candidate.charIndex, col: candidate.col, isWrapEnd: candidate.isWrapEnd };
			}
		}

		if (best) {
			this.cursor = best.charIndex;
			this.cursorAtWrapEnd = best.isWrapEnd;
			this.preferredVisualCol = null;
		}
	}

	invalidate(): void {
		// No cached state to invalidate currently
	}

	render(width: number): string[] {
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

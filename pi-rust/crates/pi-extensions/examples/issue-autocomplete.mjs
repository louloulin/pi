/**
 * `#`-triggered issue completion — the LUM-1448 end-to-end fixture.
 *
 * Provenance: this is the upstream `github-issue-autocomplete` example
 * (`packages/coding-agent/examples/extensions/github-issue-autocomplete.ts`,
 * vendored here as plain JS so the Rust host can evaluate it). The provider
 * shape is kept verbatim from upstream:
 *
 *   - `ctx.ui.addAutocompleteProvider((current) => provider)` registered from a
 *     `session_start` handler;
 *   - `triggerCharacters: ["#"]`;
 *   - `getSuggestions` matches the token with the upstream regex
 *     `/(?:^|[ \t])#([^\s#]*)$/`, returns `{ items, prefix }`, and delegates to
 *     `current.getSuggestions(...)` when the token does not match;
 *   - `applyCompletion` delegates to `current.applyCompletion(...)`;
 *   - `shouldTriggerFileCompletion` delegates to
 *     `current.shouldTriggerFileCompletion?.(...) ?? true`.
 *
 * Two deliberate deltas from the upstream file, both required to keep this
 * fixture offline and inside the port's phase-1 contract (see
 * `docs/LUM1448_AUTOCOMPLETE_PROVIDER.md`):
 *
 *   1. upstream shells out to `gh issue list` through `pi.exec`; here the open
 *      issues are a local constant, so the fixture needs no network, no `gh`
 *      and no git checkout;
 *   2. upstream's `getSuggestions` is `async` (it awaits that `pi.exec`); this
 *      port serves only synchronous provider callbacks, so it returns the
 *      suggestion object directly.
 *
 * Everything else — including the item shape `{ value, label, description }`
 * and the delegation to `current` — is upstream's.
 */

/** Open issues, standing in for `gh issue list --json number,title,state`. */
const ISSUES = [
	{ number: 2983, title: "Extension API for autocomplete", state: "OPEN" },
	{ number: 2753, title: "Reload stale resource settings", state: "OPEN" },
	{ number: 1448, title: "Extension-injected autocomplete provider", state: "OPEN" },
	{ number: 1436, title: "Autocomplete dropdown wheel routing", state: "OPEN" },
	{ number: 1305, title: "Autocomplete select list layout", state: "OPEN" },
	{ number: 2754, title: "Reload the session name", state: "CLOSED" },
];

const MAX_SUGGESTIONS = 20;

function extractIssueToken(textBeforeCursor) {
	const match = textBeforeCursor.match(/(?:^|[ \t])#([^\s#]*)$/);
	return match?.[1];
}

function formatIssueItem(issue) {
	return {
		value: `#${issue.number}`,
		label: `#${issue.number}`,
		description: `[${issue.state.toLowerCase()}] ${issue.title}`,
	};
}

function filterIssues(issues, query) {
	if (!query.trim()) {
		return issues.slice(0, MAX_SUGGESTIONS).map(formatIssueItem);
	}

	if (/^\d+$/.test(query)) {
		const numericMatches = issues
			.filter((issue) => String(issue.number).startsWith(query))
			.slice(0, MAX_SUGGESTIONS)
			.map(formatIssueItem);
		if (numericMatches.length > 0) {
			return numericMatches;
		}
	}

	const lower = query.toLowerCase();
	return issues
		.filter((issue) => `${issue.number} ${issue.title}`.toLowerCase().includes(lower))
		.slice(0, MAX_SUGGESTIONS)
		.map(formatIssueItem);
}

function createIssueAutocompleteProvider(current, getIssues) {
	return {
		triggerCharacters: ["#"],

		getSuggestions(lines, cursorLine, cursorCol, options) {
			void options;
			const currentLine = lines[cursorLine] ?? "";
			const textBeforeCursor = currentLine.slice(0, cursorCol);
			const token = extractIssueToken(textBeforeCursor);
			if (token === undefined) {
				return current.getSuggestions(lines, cursorLine, cursorCol, options);
			}

			const issues = getIssues();
			if (options?.signal?.aborted || !issues || issues.length === 0) {
				return current.getSuggestions(lines, cursorLine, cursorCol, options);
			}

			const suggestions = filterIssues(issues, token);
			if (suggestions.length === 0) {
				return current.getSuggestions(lines, cursorLine, cursorCol, options);
			}

			return {
				items: suggestions,
				prefix: `#${token}`,
			};
		},

		applyCompletion(lines, cursorLine, cursorCol, item, prefix) {
			return current.applyCompletion(lines, cursorLine, cursorCol, item, prefix);
		},

		shouldTriggerFileCompletion(lines, cursorLine, cursorCol) {
			return current.shouldTriggerFileCompletion?.(lines, cursorLine, cursorCol) ?? true;
		},
	};
}

export default function (pi) {
	pi.on("session_start", (_event, ctx) => {
		ctx.ui.addAutocompleteProvider((current) =>
			createIssueAutocompleteProvider(current, () => ISSUES),
		);
	});
}

// Pure-JS values of the `@earendil-works/kiana-ai` virtual namespace (design §8.2).
//
// Evaluated once by `virtual_modules::install`; the exported values are copied onto the
// `__kiana_virtual["@earendil-works/kiana-ai"]` namespace object, and `js/virtual/kiana-ai.js` re-exports
// them from there (so the ESM and CJS views stay identical).
//
// Ports, verbatim in behaviour:
//   - packages/ai/src/utils/typebox-helpers.ts  -> StringEnum
//   - packages/ai/src/utils/uuid.ts             -> uuidv7
//   - packages/ai/src/utils/event-stream.ts     -> EventStream / AssistantMessageEventStream
//   - packages/ai/src/utils/json-parse.ts       -> repairJson / parseJsonWithRepair /
//                                                  parseToolCallArguments
//   - packages/ai/src/utils/streaming-json.ts   -> StreamingJsonParser / ToolCallArgumentsStream
//
// No host bridge is involved: these are the same pure-JS algorithms the Node build runs.

import { Type } from "typebox/build/index.mjs";

export { Type };

// ---------------------------------------------------------------------------
// StringEnum (typebox-helpers.ts)
// ---------------------------------------------------------------------------

export function StringEnum(values, options) {
	return Type.Unsafe({
		type: "string",
		enum: values,
		...(options?.description && { description: options.description }),
		...(options?.default && { default: options.default }),
	});
}

// ---------------------------------------------------------------------------
// uuidv7 (uuid.ts)
// ---------------------------------------------------------------------------

const MAX_UUID_V7_TIMESTAMP = 0xffffffffffff;
const MAX_SEQUENCE = (1n << 41n) - 1n;

let lastOrdinaryTimestamp = -1;
let sequence;

/** Generate a time-ordered UUIDv7. A supplied timestamp is preserved for follower ids. */
export function uuidv7(timestampMs) {
	const requestedTimestamp = timestampMs ?? Date.now();
	if (
		!Number.isInteger(requestedTimestamp) ||
		requestedTimestamp < 0 ||
		requestedTimestamp > MAX_UUID_V7_TIMESTAMP
	) {
		throw new RangeError(
			`UUIDv7 timestamp must be an integer between 0 and ${MAX_UUID_V7_TIMESTAMP}`,
		);
	}

	const effectiveTimestamp =
		timestampMs === undefined ? Math.max(requestedTimestamp, lastOrdinaryTimestamp) : timestampMs;
	if (timestampMs === undefined) lastOrdinaryTimestamp = effectiveTimestamp;

	const bytes = new Uint8Array(16);
	globalThis.crypto.getRandomValues(bytes);
	if (sequence === undefined) {
		sequence =
			(BigInt(bytes[1]) << 32n) |
			(BigInt(bytes[2]) << 24n) |
			(BigInt(bytes[3]) << 16n) |
			(BigInt(bytes[4]) << 8n) |
			BigInt(bytes[5]);
	} else {
		if (sequence === MAX_SEQUENCE) throw new RangeError("UUIDv7 generator sequence exhausted");
		sequence++;
	}

	const timestamp = BigInt(effectiveTimestamp);
	for (let index = 5; index >= 0; index--) {
		bytes[index] = Number(timestamp >> BigInt((5 - index) * 8)) & 0xff;
	}
	bytes[6] = 0x70 | Number((sequence >> 37n) & 0x0fn);
	bytes[7] = Number((sequence >> 29n) & 0xffn);
	bytes[8] = 0x80 | Number((sequence >> 23n) & 0x3fn);
	bytes[9] = Number((sequence >> 15n) & 0xffn);
	bytes[10] = Number((sequence >> 7n) & 0xffn);
	bytes[11] = Number((sequence & 0x7fn) << 1n) | (bytes[11] & 0x01);

	const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0"));
	return `${hex.slice(0, 4).join("")}-${hex.slice(4, 6).join("")}-${hex.slice(6, 8).join("")}-${hex.slice(8, 10).join("")}-${hex.slice(10).join("")}`;
}

// ---------------------------------------------------------------------------
// EventStream (event-stream.ts)
// ---------------------------------------------------------------------------

class FifoQueue {
	incoming = [];
	outgoing = [];

	get length() {
		return this.incoming.length + this.outgoing.length;
	}

	enqueue(value) {
		this.incoming.push(value);
	}

	dequeue() {
		if (this.outgoing.length === 0) {
			while (this.incoming.length > 0) {
				this.outgoing.push(this.incoming.pop());
			}
		}
		return this.outgoing.pop();
	}
}

// Generic event stream class for async iteration
export class EventStream {
	#queue = new FifoQueue();
	#waiting = new FifoQueue();
	done = false;
	#finalResultPromise;
	#resolveFinalResult;
	#isComplete;
	#extractResult;

	constructor(isComplete, extractResult) {
		this.#isComplete = isComplete;
		this.#extractResult = extractResult;
		this.#finalResultPromise = new Promise((resolve) => {
			this.#resolveFinalResult = resolve;
		});
	}

	push(event) {
		if (this.done) return;

		if (this.#isComplete(event)) {
			this.done = true;
			this.#resolveFinalResult(this.#extractResult(event));
		}

		// Deliver to waiting consumer or queue it
		const waiter = this.#waiting.dequeue();
		if (waiter) {
			waiter({ value: event, done: false });
		} else {
			this.#queue.enqueue(event);
		}
	}

	end(result) {
		this.done = true;
		if (result !== undefined) {
			this.#resolveFinalResult(result);
		}
		// Notify all waiting consumers that we're done
		while (this.#waiting.length > 0) {
			const waiter = this.#waiting.dequeue();
			waiter({ value: undefined, done: true });
		}
	}

	async *[Symbol.asyncIterator]() {
		while (true) {
			if (this.#queue.length > 0) {
				yield this.#queue.dequeue();
			} else if (this.done) {
				return;
			} else {
				const result = await new Promise((resolve) => this.#waiting.enqueue(resolve));
				if (result.done) return;
				yield result.value;
			}
		}
	}

	result() {
		return this.#finalResultPromise;
	}
}

/**
 * Event stream of one assistant response. It also times the response: the final message (`done` or
 * `error` event, or the result passed to `end()`) gets `durationMs`, measured with a monotonic
 * clock from the stream's creation, unless the message already has one or its `timestamp` predates
 * the stream.
 */
export class AssistantMessageEventStream extends EventStream {
	#startedAt = Date.now();
	#startedAtMonotonic = performance.now();

	constructor() {
		super(
			(event) => event.type === "done" || event.type === "error",
			(event) => {
				if (event.type === "done") {
					return event.message;
				} else if (event.type === "error") {
					return event.error;
				}
				throw new Error("Unexpected event type for final result");
			},
		);
	}

	push(event) {
		if (event.type === "done") this.#time(event.message);
		else if (event.type === "error") this.#time(event.error);
		super.push(event);
	}

	end(result) {
		if (result !== undefined) this.#time(result);
		super.end(result);
	}

	#time(message) {
		if (
			this.done ||
			message.durationMs !== undefined ||
			message.timestamp < this.#startedAt
		)
			return;
		message.durationMs = Math.max(0, Math.round(performance.now() - this.#startedAtMonotonic));
	}
}

/** Factory function for AssistantMessageEventStream (for use in extensions) */
export function createAssistantMessageEventStream() {
	return new AssistantMessageEventStream();
}

// ---------------------------------------------------------------------------
// JSON repair + finished tool-call arguments (json-parse.ts)
// ---------------------------------------------------------------------------

const VALID_JSON_ESCAPES = new Set(['"', "\\", "/", "b", "f", "n", "r", "t"]);

function isControlCharacter(char) {
	const codePoint = char.codePointAt(0);
	return codePoint !== undefined && codePoint >= 0x00 && codePoint <= 0x1f;
}

function escapeControlCharacter(char) {
	switch (char) {
		case "\b":
			return "\\b";
		case "\f":
			return "\\f";
		case "\n":
			return "\\n";
		case "\r":
			return "\\r";
		case "\t":
			return "\\t";
		default:
			return `\\u${char.codePointAt(0)?.toString(16).padStart(4, "0") ?? "0000"}`;
	}
}

/**
 * Repairs malformed JSON string literals by:
 * - escaping raw control characters inside strings
 * - doubling backslashes before invalid escape characters
 */
export function repairJson(json) {
	let repaired = "";
	let inString = false;

	for (let index = 0; index < json.length; index++) {
		const char = json[index];

		if (!inString) {
			repaired += char;
			if (char === '"') {
				inString = true;
			}
			continue;
		}

		if (char === '"') {
			repaired += char;
			inString = false;
			continue;
		}

		if (char === "\\") {
			const nextChar = json[index + 1];
			if (nextChar === undefined) {
				repaired += "\\\\";
				continue;
			}

			if (nextChar === "u") {
				const unicodeDigits = json.slice(index + 2, index + 6);
				if (/^[0-9a-fA-F]{4}$/.test(unicodeDigits)) {
					repaired += `\\u${unicodeDigits}`;
					index += 5;
					continue;
				}
			}

			if (VALID_JSON_ESCAPES.has(nextChar)) {
				repaired += `\\${nextChar}`;
				index += 1;
				continue;
			}

			repaired += "\\\\";
			continue;
		}

		repaired += isControlCharacter(char) ? escapeControlCharacter(char) : char;
	}

	return repaired;
}

export function parseJsonWithRepair(json) {
	try {
		return JSON.parse(json);
	} catch (error) {
		const repairedJson = repairJson(json);
		if (repairedJson !== json) {
			return JSON.parse(repairedJson);
		}
		throw error;
	}
}

/**
 * Parses the model's complete arguments text of a finished tool call. Text that is not a valid
 * JSON object means the model sent malformed arguments: `arguments` is `{}` and
 * `argumentsError` describes the problem, including the text the model sent.
 */
export function parseToolCallArguments(json) {
	if (!json?.trim()) return { arguments: {} };
	try {
		const parsed = parseJsonWithRepair(json);
		if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
			throw new Error("Tool arguments must be a JSON object");
		}
		return { arguments: parsed };
	} catch (error) {
		return {
			arguments: {},
			argumentsError: `${error instanceof Error ? error.message : String(error)}\n\nReceived arguments:\n${json}`,
		};
	}
}

// ---------------------------------------------------------------------------
// Streaming JSON parser (streaming-json.ts)
// ---------------------------------------------------------------------------

/** Where a value sits in its parent; undefined for a value in an invalid position. */
// Slot shapes: { kind: "root" } | { kind: "member", key } | { kind: "element", index }

// Changes are reported at most this deep, so a path costs O(1) to build.
const MAX_REPORTED_DEPTH = 32;

const ROOT_SLOT = { kind: "root" };
// Copy credit per append and per appended character. The floor keeps small documents fresh at every snapshot.
const CREDIT_PER_APPEND = 256;
const CREDIT_PER_CHARACTER = 8;
// An unterminated number longer than this is not shown until it ends, so `snapshot()` stays cheap.
const MAX_PENDING_SCALAR_LENGTH = 64;
const NUMBER = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?$/;
const NUMBER_WITHOUT_EXPONENT_DIGITS = /^(-?(?:0|[1-9]\d*)(?:\.\d+)?)[eE][+-]?$/;
const LITERALS = [
	["true", true],
	["false", false],
	["null", null],
];

const QUOTE = 0x22;
const BACKSLASH = 0x5c;

/** Incremental, tolerant JSON parser; see the TS module comment for its rules and costs. */
export class StreamingJsonParser {
	#root = undefined;
	#done = false;
	#stack = [];
	#string = undefined;
	#scalar = "";
	#scalarSlot = undefined;
	/** Sum of `size + 1` over the open path: what a fresh snapshot copies. */
	#pathCost = 0;
	#credit = 0;
	#dirty = false;
	#snapshot = undefined;
	/** Changes of the piece being read. */
	#changes = [];

	/**
	 * Read the next piece of text; returns the changes it made, in order. Costs time linear in
	 * `text.length`.
	 */
	append(text) {
		this.#credit += CREDIT_PER_APPEND + CREDIT_PER_CHARACTER * text.length;
		this.#changes = [];
		if (this.#string !== undefined) this.#string.change = undefined;
		let index = 0;
		while (index < text.length && !this.#done) {
			const open = this.#string;
			if (open !== undefined) index = this.#readString(open, text, index);
			else if (this.#scalar !== "") index = this.#readScalar(text, index);
			else this.#structural(text[index++]);
		}
		return this.#changes;
	}

	/**
	 * The value of the text read so far: `undefined` before a value starts, otherwise a plain JSON
	 * value that never changes afterwards.
	 */
	snapshot() {
		if (!this.#dirty) return this.#snapshot;
		const pending = this.#pendingScalar();
		const cost = this.#pathCost + (pending === undefined ? 0 : 1);
		if (cost > this.#credit) return this.#snapshot;
		this.#credit -= cost;
		this.#dirty = false;
		this.#snapshot = this.#copyOpenPath(pending);
		return this.#snapshot;
	}

	/** Read string characters from `start` until the string ends or the text does. */
	#readString(open, text, start) {
		let runStart = start;
		for (let index = start; index < text.length; index++) {
			const code = text.charCodeAt(index);
			if (open.escape !== "") {
				if (this.#continueEscape(open, text[index])) {
					runStart = index + 1;
					continue;
				}
				// Not part of the escape: read it as a normal string character below.
				runStart = index;
			}
			if (code === QUOTE) {
				if (index > runStart) this.#appendString(open, text.slice(runStart, index));
				this.#endString(open);
				return index + 1;
			}
			if (code === BACKSLASH) {
				if (index > runStart) this.#appendString(open, text.slice(runStart, index));
				open.escape = "\\";
				runStart = index + 1;
			}
		}
		if (text.length > runStart) this.#appendString(open, text.slice(runStart));
		return text.length;
	}

	/** Read number or literal characters from `start` up to the next delimiter. */
	#readScalar(text, start) {
		let end = start;
		while (end < text.length && !isDelimiter(text[end])) end++;
		if (end > start) {
			this.#scalar += text.slice(start, end);
			if (this.#scalarSlot !== undefined) this.#dirty = true;
		}
		if (end < text.length) this.#endScalar();
		return end;
	}

	/** Handle one character outside strings. */
	#structural(char) {
		switch (char) {
			case " ":
			case "\t":
			case "\n":
			case "\r":
				return;
			case '"':
				this.#startString();
				return;
			case "{":
			case "[":
				this.#openContainer(char === "{" ? {} : []);
				return;
			case "}":
			case "]":
				this.#closeContainer();
				return;
			case ":": {
				const frame = this.#stack.at(-1);
				if (
					frame !== undefined &&
					!Array.isArray(frame.container) &&
					frame.phase === "colon"
				)
					frame.phase = "value";
				return;
			}
			case ",": {
				const frame = this.#stack.at(-1);
				if (frame !== undefined)
					frame.phase = Array.isArray(frame.container) ? "value" : "key";
				return;
			}
			default:
				this.#scalar = char;
				this.#scalarSlot = this.#valueSlot();
				if (this.#scalarSlot !== undefined) this.#dirty = true;
		}
	}

	/** Where a value starting now goes, or `undefined` when this position takes no value. */
	#valueSlot() {
		const frame = this.#stack.at(-1);
		if (frame === undefined) return this.#root === undefined ? ROOT_SLOT : undefined;
		if (Array.isArray(frame.container))
			return { kind: "element", index: frame.container.length };
		// A missing colon is tolerated: `{"a" 1}` reads as `{"a": 1}`.
		if ((frame.phase === "value" || frame.phase === "colon") && frame.key !== undefined) {
			return { kind: "member", key: frame.key };
		}
		return undefined;
	}

	#startString() {
		const frame = this.#stack.at(-1);
		// A missing comma is tolerated: `{"a": 1 "b": 2}` reads the second member.
		if (
			frame !== undefined &&
			!Array.isArray(frame.container) &&
			(frame.phase === "key" || frame.phase === "comma")
		) {
			this.#string = {
				target: "key",
				slot: undefined,
				text: "",
				escape: "",
				path: undefined,
				change: undefined,
			};
			return;
		}
		const slot = this.#valueSlot();
		const path = this.#reportedPath(slot);
		this.#string = { target: "value", slot, text: "", escape: "", path, change: undefined };
		this.#place(slot, "");
		if (path !== undefined) this.#string.change = this.#report({ kind: "set", path, value: "" });
	}

	/** The path of a value at `slot` in the innermost open container, when reported. */
	#reportedPath(slot) {
		if (slot === undefined) return undefined;
		if (slot.kind === "root") return [];
		const frame = this.#stack.at(-1);
		if (!frame.reportsInside) return undefined;
		return [...frame.path, slot.kind === "member" ? slot.key : slot.index];
	}

	#report(change) {
		this.#changes.push(change);
		return change;
	}

	/** Continue an escape with `char`; returns false when `char` is not part of it. */
	#continueEscape(open, char) {
		if (open.escape === "\\") {
			const decoded = SIMPLE_ESCAPES[char];
			if (decoded !== undefined) {
				open.escape = "";
				this.#appendString(open, decoded);
				return true;
			}
			if (char === "u") {
				open.escape = "\\u";
				return true;
			}
			// Invalid escape: keep the backslash as text and read the character normally.
			open.escape = "";
			this.#appendString(open, "\\");
			return false;
		}
		if (isHexDigit(char)) {
			open.escape += char;
			if (open.escape.length < 6) return true;
			const decoded = String.fromCharCode(Number.parseInt(open.escape.slice(2), 16));
			open.escape = "";
			this.#appendString(open, decoded);
			return true;
		}
		// `\u` without four hex digits: keep it as text and read the character normally.
		const literal = open.escape;
		open.escape = "";
		this.#appendString(open, literal);
		return false;
	}

	#appendString(open, text) {
		open.text += text;
		if (open.target !== "value") return;
		this.#place(open.slot, open.text);
		if (open.path === undefined) return;
		// Extend this string's newest change while nothing came after it; otherwise report an append.
		const change = open.change;
		if (change !== undefined && this.#changes.at(-1) === change) {
			if (change.kind === "set") change.value = change.value + text;
			else change.text += text;
			return;
		}
		open.change = this.#report({ kind: "append", path: open.path, text });
	}

	#endString(open) {
		this.#string = undefined;
		const frame = this.#stack.at(-1);
		if (open.target === "key") {
			if (frame === undefined) return;
			frame.key = open.text;
			frame.phase = "colon";
			return;
		}
		this.#valueEnded(frame);
	}

	#endScalar() {
		const token = this.#scalar;
		const slot = this.#scalarSlot;
		this.#scalar = "";
		this.#scalarSlot = undefined;
		const value = scalarValue(token);
		if (value !== undefined) {
			this.#place(slot, value);
			const path = this.#reportedPath(slot);
			if (path !== undefined) this.#report({ kind: "set", path, value });
		}
		this.#valueEnded(this.#stack.at(-1));
	}

	/** The value of the unterminated scalar at the end of the text, if it shows. */
	#pendingScalar() {
		const slot = this.#scalarSlot;
		const token = this.#scalar;
		if (slot === undefined || token === "" || token.length > MAX_PENDING_SCALAR_LENGTH)
			return undefined;
		const value = pendingScalarValue(token);
		return value === undefined ? undefined : { slot, value };
	}

	#openContainer(container) {
		const slot = this.#valueSlot();
		const path = this.#reportedPath(slot);
		this.#place(slot, container);
		this.#stack.push({
			container,
			slot,
			path,
			reportsInside: path !== undefined && path.length < MAX_REPORTED_DEPTH,
			size: 0,
			key: undefined,
			phase: Array.isArray(container) ? "value" : "key",
		});
		this.#pathCost += 1;
		this.#dirty = true;
		// A fresh container: the working one keeps changing, and a change never does.
		if (path !== undefined)
			this.#report({ kind: "set", path, value: Array.isArray(container) ? [] : {} });
	}

	#closeContainer() {
		const frame = this.#stack.pop();
		// A stray closer outside any container is ignored.
		if (frame === undefined) return;
		this.#pathCost -= frame.size + 1;
		this.#dirty = true;
		// Too deep to report inside: the whole value, final now that it is closed.
		if (frame.path !== undefined && !frame.reportsInside) {
			this.#report({ kind: "set", path: frame.path, value: frame.container });
		}
		this.#valueEnded(this.#stack.at(-1));
	}

	/** A value in `frame`, or the root value when there is no frame, has ended. */
	#valueEnded(frame) {
		if (frame === undefined) {
			// Text after the root value is ignored, but only once a root value exists.
			if (this.#root !== undefined) this.#done = true;
			return;
		}
		frame.phase = "comma";
	}

	/** Set the value at `slot` in the open path; nothing for a value in an invalid position. */
	#place(slot, value) {
		if (slot === undefined) return;
		this.#dirty = true;
		if (slot.kind === "root") {
			this.#root = value;
			return;
		}
		const frame = this.#stack.at(-1);
		if (slot.kind === "element") {
			const elements = frame.container;
			if (slot.index === elements.length) {
				frame.size++;
				this.#pathCost++;
			}
			elements[slot.index] = value;
			return;
		}
		const members = frame.container;
		if (!Object.hasOwn(members, slot.key)) {
			frame.size++;
			this.#pathCost++;
		}
		setMember(members, slot.key, value);
	}

	/** A fresh snapshot: copies of the open containers, innermost first. */
	#copyOpenPath(pending) {
		if (this.#stack.length === 0)
			return pending?.slot.kind === "root" ? pending.value : this.#root;
		let child;
		let childSlot;
		for (let depth = this.#stack.length - 1; depth >= 0; depth--) {
			const frame = this.#stack[depth];
			const copy = Array.isArray(frame.container)
				? frame.container.slice()
				: { ...frame.container };
			if (depth === this.#stack.length - 1 && pending !== undefined)
				setSlot(copy, pending.slot, pending.value);
			if (child !== undefined && childSlot !== undefined) setSlot(copy, childSlot, child);
			child = copy;
			childSlot = frame.slot;
		}
		// The outermost open container is the root unless it sits in an invalid position.
		return childSlot?.kind === "root" ? child : this.#root;
	}
}

/**
 * The arguments of one streaming tool call: the raw text, for the one full parse when the call
 * ends, and a `StreamingJsonParser` for the partial arguments in between.
 */
export class ToolCallArgumentsStream {
	#text = "";
	#parser = new StreamingJsonParser();
	#snapshot = undefined;
	#arguments = {};

	/** The raw argument text received so far. */
	get text() {
		return this.#text;
	}

	/** Append a piece of argument text; returns the partial arguments. */
	append(delta) {
		this.#text += delta;
		this.#parser.append(delta);
		return this.#partial();
	}

	/**
	 * Take the provider's complete argument text. When it continues the text so far, returns the
	 * missing piece as `delta`; otherwise starts over with `text`.
	 */
	replace(text) {
		if (text.startsWith(this.#text)) {
			const delta = text.slice(this.#text.length);
			return { delta, arguments: delta === "" ? this.#partial() : this.append(delta) };
		}
		this.#text = "";
		this.#parser = new StreamingJsonParser();
		return { delta: undefined, arguments: this.append(text) };
	}

	/** The final arguments: one full parse of the complete text. */
	finish() {
		return parseToolCallArguments(this.#text);
	}

	#partial() {
		const snapshot = this.#parser.snapshot();
		if (snapshot === this.#snapshot) return this.#arguments;
		this.#snapshot = snapshot;
		// Tool arguments are an object; any other partial value shows as no arguments yet.
		this.#arguments =
			typeof snapshot === "object" && snapshot !== null && !Array.isArray(snapshot)
				? snapshot
				: {};
		return this.#arguments;
	}
}

const SIMPLE_ESCAPES = {
	'"': '"',
	"\\": "\\",
	"/": "/",
	b: "\b",
	f: "\f",
	n: "\n",
	r: "\r",
	t: "\t",
};

function isDelimiter(char) {
	return (
		char === " " ||
		char === "\t" ||
		char === "\n" ||
		char === "\r" ||
		char === "," ||
		char === ":" ||
		char === "{" ||
		char === "}" ||
		char === "[" ||
		char === "]" ||
		char === '"'
	);
}

function isHexDigit(char) {
	return (char >= "0" && char <= "9") || (char >= "a" && char <= "f") || (char >= "A" && char <= "F");
}

/** The value of a complete number or literal token; `undefined` for anything else. */
function scalarValue(token) {
	for (const [literal, value] of LITERALS) if (token === literal) return value;
	return NUMBER.test(token) ? Number(token) : undefined;
}

/** The value an unterminated number or literal token shows. */
function pendingScalarValue(token) {
	for (const [literal, value] of LITERALS) if (literal.startsWith(token)) return value;
	if (NUMBER.test(token)) return Number(token);
	const mantissa = NUMBER_WITHOUT_EXPONENT_DIGITS.exec(token);
	return mantissa === null ? undefined : Number(mantissa[1]);
}

function setSlot(container, slot, value) {
	if (slot.kind === "element") container[slot.index] = value;
	else if (slot.kind === "member") setMember(container, slot.key, value);
}

function setMember(members, key, value) {
	// `__proto__` must become an own member, as in JSON.parse(), not the prototype.
	if (key === "__proto__") {
		Object.defineProperty(members, key, {
			value,
			writable: true,
			enumerable: true,
			configurable: true,
		});
	} else {
		members[key] = value;
	}
}

// ---------------------------------------------------------------------------
// Static constants re-exported by the compat entrypoint (copied from the TS
// module; `virtual_modules` tests compare them with the kiana-ai Rust constants).
// ---------------------------------------------------------------------------

export const KIANA_AI_VERSION = "1.4.0";

export const DEFAULT_MAX_AGENT_RETRY_DELAY_MS = 60000;

export const ANTHROPIC_API_KEY_ENV = "ANTHROPIC_API_KEY";
export const ANTHROPIC_AUTH_TOKEN_ENV = "ANTHROPIC_AUTH_TOKEN";
export const ANTHROPIC_FEDERATION_RULE_ID_ENV = "ANTHROPIC_FEDERATION_RULE_ID";
export const ANTHROPIC_IDENTITY_TOKEN_FILE_ENV = "ANTHROPIC_IDENTITY_TOKEN_FILE";
export const ANTHROPIC_OAUTH_TOKEN_ENV = "ANTHROPIC_OAUTH_TOKEN";
export const ANTHROPIC_ORGANIZATION_ID_ENV = "ANTHROPIC_ORGANIZATION_ID";
export const ANTHROPIC_SERVICE_ACCOUNT_ID_ENV = "ANTHROPIC_SERVICE_ACCOUNT_ID";
export const ANTHROPIC_WORKSPACE_ID_ENV = "ANTHROPIC_WORKSPACE_ID";

export const KIANA_ATTRIBUTION = {
	name: "kiana",
	url: "https://kiana.dev",
	categories: ["cli-agent"],
};

export const AI_TELEMETRY_SCHEMA = {
	"version": 1,
	"spans": {
		"kiana.ai.request": {
			"description": "One logical model request, including authentication and provider setup.",
			"parents": {
				"kind": "any"
			},
			"startAttributes": {
				"provider": {
					"type": "string",
					"description": "Provider identifier.",
					"required": true
				},
				"api": {
					"type": "string",
					"description": "API identifier.",
					"required": true
				},
				"model": {
					"type": "string",
					"description": "Requested model identifier.",
					"required": true
				},
				"reasoning": {
					"type": "string",
					"description": "Scalar requested reasoning level.",
					"required": false
				}
			},
			"endAttributes": {
				"stop": {
					"type": "string",
					"description": "Normalized stop reason.",
					"values": [
						"pending",
						"stop",
						"length",
						"toolUse",
						"error",
						"aborted",
						"deferred"
					]
				},
				"usage.input": {
					"type": "number",
					"description": "Uncached input tokens."
				},
				"usage.output": {
					"type": "number",
					"description": "Output tokens."
				},
				"usage.cache_read": {
					"type": "number",
					"description": "Cache-read tokens."
				},
				"usage.cache_write": {
					"type": "number",
					"description": "Cache-write tokens."
				},
				"cost.total": {
					"type": "number",
					"description": "Total request cost in USD."
				},
				"response.id": {
					"type": "string",
					"description": "Provider response identifier.",
					"cardinality": "high"
				}
			},
			"events": {
				"kiana.ai.first_chunk": {
					"description": "First normalized stream event delivered.",
					"attributes": {}
				},
				"kiana.ai.block": {
					"description": "A completed content block; content is never recorded.",
					"attributes": {
						"type": {
							"type": "string",
							"description": "Block kind.",
							"values": [
								"text",
								"thinking",
								"tool_call"
							],
							"required": true
						},
						"index": {
							"type": "number",
							"description": "Content block index.",
							"required": true
						},
						"chars": {
							"type": "number",
							"description": "UTF-16 character count.",
							"required": true
						},
						"tool": {
							"type": "string",
							"description": "Tool name, for tool-call blocks only.",
							"required": false
						}
					}
				}
			},
			"status": {
				"default": "ok",
				"errorWhen": "Stop reason is error: provider_error with at most 500 error-message characters. Aborted stays ok."
			}
		},
		"kiana.ai.poll": {
			"description": "One deferred-response poll, not the deferred request's lifetime.",
			"parents": {
				"kind": "any"
			},
			"startAttributes": {
				"provider": {
					"type": "string",
					"description": "Provider identifier.",
					"required": true
				},
				"api": {
					"type": "string",
					"description": "API identifier.",
					"required": true
				},
				"model": {
					"type": "string",
					"description": "Requested model identifier.",
					"required": true
				},
				"reasoning": {
					"type": "string",
					"description": "Scalar requested reasoning level.",
					"required": false
				}
			},
			"endAttributes": {
				"stop": {
					"type": "string",
					"description": "Normalized stop reason.",
					"values": [
						"pending",
						"stop",
						"length",
						"toolUse",
						"error",
						"aborted",
						"deferred"
					]
				},
				"usage.input": {
					"type": "number",
					"description": "Uncached input tokens."
				},
				"usage.output": {
					"type": "number",
					"description": "Output tokens."
				},
				"usage.cache_read": {
					"type": "number",
					"description": "Cache-read tokens."
				},
				"usage.cache_write": {
					"type": "number",
					"description": "Cache-write tokens."
				},
				"cost.total": {
					"type": "number",
					"description": "Total request cost in USD."
				},
				"response.id": {
					"type": "string",
					"description": "Provider response identifier.",
					"cardinality": "high"
				}
			},
			"events": {
				"kiana.ai.first_chunk": {
					"description": "First normalized stream event delivered.",
					"attributes": {}
				},
				"kiana.ai.block": {
					"description": "A completed content block; content is never recorded.",
					"attributes": {
						"type": {
							"type": "string",
							"description": "Block kind.",
							"values": [
								"text",
								"thinking",
								"tool_call"
							],
							"required": true
						},
						"index": {
							"type": "number",
							"description": "Content block index.",
							"required": true
						},
						"chars": {
							"type": "number",
							"description": "UTF-16 character count.",
							"required": true
						},
						"tool": {
							"type": "string",
							"description": "Tool name, for tool-call blocks only.",
							"required": false
						}
					}
				}
			},
			"status": {
				"default": "ok",
				"errorWhen": "Stop reason is error: provider_error with at most 500 error-message characters. Aborted stays ok."
			}
		}
	},
	"metrics": {
		"kiana.ai.request.duration": {
			"kind": "histogram",
			"description": "Logical request or poll duration.",
			"unit": "ms",
			"attributes": {
				"provider": {
					"type": "string",
					"description": "Provider identifier.",
					"required": true
				},
				"model": {
					"type": "string",
					"description": "Requested model identifier.",
					"required": true
				},
				"stop": {
					"type": "string",
					"description": "Normalized stop reason.",
					"values": [
						"pending",
						"stop",
						"length",
						"toolUse",
						"error",
						"aborted",
						"deferred"
					],
					"required": true
				}
			}
		},
		"kiana.ai.first_chunk.duration": {
			"kind": "histogram",
			"description": "Time to the first normalized stream event.",
			"unit": "ms",
			"attributes": {
				"provider": {
					"type": "string",
					"description": "Provider identifier.",
					"required": true
				},
				"model": {
					"type": "string",
					"description": "Requested model identifier.",
					"required": true
				}
			}
		},
		"kiana.ai.tokens": {
			"kind": "counter",
			"description": "Reported model token usage; pending and deferred polls are excluded.",
			"unit": "token",
			"attributes": {
				"provider": {
					"type": "string",
					"description": "Provider identifier.",
					"required": true
				},
				"model": {
					"type": "string",
					"description": "Requested model identifier.",
					"required": true
				},
				"kind": {
					"type": "string",
					"description": "Token usage category.",
					"values": [
						"input",
						"output",
						"cache_read",
						"cache_write"
					],
					"required": true
				}
			}
		},
		"kiana.ai.cost": {
			"kind": "counter",
			"description": "Reported model cost; pending and deferred polls are excluded.",
			"unit": "USD",
			"attributes": {
				"provider": {
					"type": "string",
					"description": "Provider identifier.",
					"required": true
				},
				"model": {
					"type": "string",
					"description": "Requested model identifier.",
					"required": true
				}
			}
		}
	}
};

// ---------------------------------------------------------------------------
// utils/text.ts
// ---------------------------------------------------------------------------

/** Extract and join text from message content. */
export function contentText(content, separator = "\n") {
	if (typeof content === "string") return content;
	return content
		.filter((block) => block.type === "text")
		.map((block) => block.text)
		.join(separator);
}

/** Render a system message as a complete prompt: its content followed by its sections. */
export function getSystemMessageText(message) {
	const parts = [contentText(message.content)];
	for (const text of Object.values(message.sections ?? {})) {
		if (text !== null) parts.push(text);
	}
	return parts.filter((part) => part.length > 0).join("\n\n");
}

/**
 * Render a later system message for APIs that accept system messages mid-conversation.
 * Section changes are framed by name so the model can relate them to the leading prompt.
 */
export function renderSystemMessageUpdate(message) {
	const parts = [];
	const text = contentText(message.content);
	if (text.length > 0) parts.push(text);
	for (const [name, value] of Object.entries(message.sections ?? {})) {
		parts.push(
			value === null
				? `Removed system prompt section "${name}".`
				: `Updated system prompt section "${name}":\n\n${value}`,
		);
	}
	return parts.join("\n\n");
}

// ---------------------------------------------------------------------------
// utils/transcript.ts
// ---------------------------------------------------------------------------

/**
 * Build the leading system message for a prompt and tool set. Returns undefined when
 * both are empty, so an empty transcript stays empty.
 */
export function createInitialSystemMessage(systemPrompt, tools) {
	const hasSystemPrompt = systemPrompt !== undefined && systemPrompt.length > 0;
	const hasTools = tools !== undefined && tools.length > 0;
	if (!hasSystemPrompt && !hasTools) return undefined;
	return {
		role: "system",
		content: systemPrompt ?? "",
		...(hasTools ? { toolsAdded: tools } : {}),
		timestamp: 0,
	};
}

/**
 * Fold `Context.systemPrompt` and `Context.tools` into a leading system message.
 * This is the only entry point that produces a transcript.
 */
export function normalizeContext(context) {
	const initialMessage = createInitialSystemMessage(context.systemPrompt, context.tools);
	const messages = initialMessage ? [initialMessage, ...context.messages] : context.messages;
	return { messages };
}

function isSystemMessage(message) {
	return message.role === "system";
}

/** Return the leading system message, if the transcript starts with one. */
export function getInitialSystemMessage(messages) {
	const first = messages[0];
	return first && isSystemMessage(first) ? first : undefined;
}

/** Drop the leading system message for APIs that carry the prompt outside the message list. */
export function withoutInitialSystemMessage(messages) {
	return getInitialSystemMessage(messages) ? messages.slice(1) : messages;
}

/** Resolve the tools available after applying every transcript delta in order. */
export function getCurrentTools(messages) {
	const tools = new Map();
	for (const message of messages) {
		if (!isSystemMessage(message)) continue;
		for (const tool of message.toolsRemoved ?? []) tools.delete(tool.name);
		for (const tool of message.toolsAdded ?? []) tools.set(tool.name, tool);
	}
	return [...tools.values()];
}

/**
 * Replay every system message into one leading system message holding the current
 * prompt and tools.
 */
export function getCurrentSystemMessage(messages) {
	const content = [];
	const sections = new Map();
	let timestamp;
	for (const message of messages) {
		if (!isSystemMessage(message)) continue;
		timestamp ??= message.timestamp;
		const text = contentText(message.content);
		if (text.length > 0) content.push(text);
		for (const [name, value] of Object.entries(message.sections ?? {})) {
			if (value === null) sections.delete(name);
			else sections.set(name, value);
		}
	}
	const tools = getCurrentTools(messages);
	if (timestamp === undefined && tools.length === 0) return undefined;
	return {
		role: "system",
		content: content.join("\n\n"),
		...(sections.size > 0 ? { sections: Object.fromEntries(sections) } : {}),
		...(tools.length > 0 ? { toolsAdded: tools } : {}),
		timestamp: timestamp ?? 0,
	};
}

/** Render the current system prompt text after replaying every system message. */
export function getCurrentSystemPrompt(messages) {
	const message = getCurrentSystemMessage(messages);
	return message ? getSystemMessageText(message) : "";
}

/**
 * Rebuild the transcript for APIs without mid-conversation system messages: the replayed
 * system message leads, and every later system message is dropped.
 */
export function collapseSystemMessages(context) {
	const head = getCurrentSystemMessage(context.messages);
	const messages = context.messages.filter((message) => message.role !== "system");
	return { messages: head ? [head, ...messages] : messages };
}

/** Keep later system messages in place when the model accepts them; otherwise collapse them. */
export function resolveTranscript(context, supportsMidConvoSystemMessages) {
	return supportsMidConvoSystemMessages ? context : collapseSystemMessages(context);
}

/** Strip executable and display-only fields from a tool before transcript comparison or persistence. */
export function toToolDeclaration(tool) {
	return {
		name: tool.name,
		description: tool.description,
		parameters: JSON.parse(JSON.stringify(tool.parameters)),
		...(tool.constrainedSampling === undefined ? {} : { constrainedSampling: tool.constrainedSampling }),
	};
}

/** Whether two tools declare the same interface to the model. */
export function declarationsEqual(left, right) {
	return JSON.stringify(toToolDeclaration(left)) === JSON.stringify(toToolDeclaration(right));
}

/** Compare two complete tool states. A changed definition is a removal followed by an addition. */
export function getToolStateChanges(previous, current) {
	const previousTools = new Map(previous.map((tool) => [tool.name, tool]));
	const currentTools = new Map(current.map((tool) => [tool.name, tool]));
	return {
		toolsAdded: current
			.filter((tool) => {
				const previousTool = previousTools.get(tool.name);
				return previousTool === undefined || !declarationsEqual(previousTool, tool);
			})
			.map(toToolDeclaration),
		toolsRemoved: previous
			.filter((tool) => {
				const currentTool = currentTools.get(tool.name);
				return currentTool === undefined || !declarationsEqual(tool, currentTool);
			})
			.map((tool) => ({ name: tool.name })),
	};
}

/** Every definition referenced by transcript tool state, in first-declaration order. */
export function getDeclaredTools(messages) {
	const definitions = new Map();
	for (const message of messages) {
		if (!isSystemMessage(message)) continue;
		for (const tool of message.toolsAdded ?? []) definitions.set(tool.name, tool);
	}
	return [...definitions.values()];
}

/** Whether a tool name was declared twice with different definitions. */
export function hasToolRedefinitions(messages) {
	const declared = new Map();
	for (const message of messages) {
		if (!isSystemMessage(message)) continue;
		for (const tool of message.toolsAdded ?? []) {
			const previous = declared.get(tool.name);
			if (previous !== undefined && !declarationsEqual(previous, tool)) return true;
			declared.set(tool.name, tool);
		}
	}
	return false;
}

/** Index of the last system message with a removal or same-name redeclaration, or -1. */
function findLastNonAdditiveToolChange(systemMessages) {
	const declared = new Set();
	let last = -1;
	for (let index = 0; index < systemMessages.length; index++) {
		const message = systemMessages[index];
		if ((message.toolsRemoved?.length ?? 0) > 0) last = index;
		for (const tool of message.toolsAdded ?? []) {
			if (declared.has(tool.name)) last = index;
			declared.add(tool.name);
		}
	}
	return last;
}

/** Whether tool history contains a removal or same-name redeclaration an addition-only transport cannot replay. */
export function hasNonAdditiveToolChanges(messages) {
	return findLastNonAdditiveToolChange(messages.filter(isSystemMessage)) >= 0;
}

/** Split tool declarations between the top-level request field and in-place changes. */
export function resolveTranscriptTools(messages, support) {
	const systemMessages = messages.filter(isSystemMessage);
	if (support === "none") {
		return { requestTools: getCurrentTools(systemMessages), foldedSystemMessages: systemMessages.length };
	}
	if (support === "changes") return { requestTools: [], foldedSystemMessages: 0 };
	const folded = Math.max(
		getInitialSystemMessage(messages) ? 1 : 0,
		findLastNonAdditiveToolChange(systemMessages) + 1,
	);
	return { requestTools: getCurrentTools(systemMessages.slice(0, folded)), foldedSystemMessages: folded };
}

// ---------------------------------------------------------------------------
// utils/overflow.ts
// ---------------------------------------------------------------------------

const OVERFLOW_PATTERNS = [
	/prompt (?:is )?too long/i,
	/prompt exceeds max length/i,
	/request_too_large/i,
	/input is too long for requested model/i,
	/exceeds the context window/i,
	/exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))/i,
	/input token count.*exceeds the maximum/i,
	/maximum prompt length is \d+/i,
	/reduce the length of the messages/i,
	/maximum context length is \d+ tokens/i,
	/exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?/i,
	/input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)/i,
	/exceeds the limit of \d+/i,
	/exceeds the available context size/i,
	/greater than the context length/i,
	/context window exceeds limit/i,
	/exceeded model token limit/i,
	/too large for model with \d+ maximum context length/i,
	/prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?/i,
	/model_context_window_exceeded/i,
	/prompt too long; exceeded (?:max )?context length/i,
	/range of input length should be/i,
	/context[_ ]length[_ ]exceeded/i,
	/too many tokens/i,
	/token limit exceeded/i,
	/^(?:[\w -]+ API error \(413\):|413(?:\s|:|$))/i,
];

const CEREBRAS_BODYLESS_OVERFLOW_PATTERN = /^4(?:00|13)\s*(?:status code)?\s*\(no body\)/i;

const NON_OVERFLOW_PATTERNS = [
	/^(Throttling error|Service unavailable):/i,
	/rate limit/i,
	/too many requests/i,
];

/** Get the overflow patterns for testing purposes. */
export function getOverflowPatterns() {
	return [...OVERFLOW_PATTERNS];
}

/** Check if an assistant message represents a context overflow error. */
export function isContextOverflow(message, contextWindow) {
	if (message.stopReason === "error" && message.errorMessage) {
		const isNonOverflow = NON_OVERFLOW_PATTERNS.some((pattern) => pattern.test(message.errorMessage));
		if (!isNonOverflow) {
			if (OVERFLOW_PATTERNS.some((pattern) => pattern.test(message.errorMessage))) return true;
			if (
				message.provider === "cerebras" &&
				CEREBRAS_BODYLESS_OVERFLOW_PATTERN.test(message.errorMessage)
			) {
				return true;
			}
		}
	}

	if (contextWindow && message.stopReason === "stop") {
		const inputTokens = message.usage.input + message.usage.cacheRead;
		if (inputTokens > contextWindow) return true;
	}

	if (contextWindow && message.stopReason === "length" && message.usage.output === 0) {
		const inputTokens = message.usage.input + message.usage.cacheRead;
		if (inputTokens >= contextWindow * 0.99) return true;
	}

	return false;
}

/** Check whether a length stop ended below the caller or model's intended output limit. */
export function isRecoverableLength(message, desiredMaxOutput) {
	return message.stopReason === "length" && desiredMaxOutput > 0 && message.usage.output < desiredMaxOutput;
}

// ---------------------------------------------------------------------------
// utils/diagnostics.ts
// ---------------------------------------------------------------------------

/** `String(value)` for a thrown value, matching TS `formatThrownValue`. */
export function formatThrownValue(value) {
	if (value instanceof Error) return value.message || value.name;
	if (typeof value === "string") return value;
	return String(value);
}

/** Normalize a thrown value into a diagnostic error record. */
export function extractDiagnosticError(error) {
	if (!(error instanceof Error)) return { name: "ThrownValue", message: formatThrownValue(error) };
	const code = error.code;
	return {
		name: error.name || undefined,
		message: error.message || error.name,
		stack: error.stack,
		code: typeof code === "string" || typeof code === "number" ? code : undefined,
	};
}

/** Build an assistant message diagnostic. */
export function createAssistantMessageDiagnostic(type, error, details) {
	return { type, timestamp: Date.now(), error: extractDiagnosticError(error), details };
}

/** Append a diagnostic to an assistant message (mutates the diagnostics array). */
export function appendAssistantMessageDiagnostic(message, diagnostic) {
	message.diagnostics = [...(message.diagnostics ?? []), diagnostic];
}

// ---------------------------------------------------------------------------
// utils/identity.ts
// ---------------------------------------------------------------------------

/** Merge identities field by field; later sources win and undefined fields are skipped. */
export function mergeClientIdentity(...sources) {
	let merged;
	for (const source of sources) {
		if (!source) continue;
		merged = { ...merged };
		if (source.userAgent !== undefined) merged.userAgent = source.userAgent;
		if (source.attribution !== undefined) merged.attribution = source.attribution;
		if (source.version !== undefined) merged.version = source.version;
	}
	return merged;
}

/** The version in the detailed `User-Agent`. */
export function getClientVersion(identity) {
	return identity?.version ?? KIANA_AI_VERSION;
}

function runtimeDescription() {
	if (typeof process === "undefined") return undefined;
	const versions = process.versions;
	if (versions?.bun) return `bun/${versions.bun}`;
	if (versions?.node) return `node/${process.version}`;
	return undefined;
}

/** The `User-Agent` value of an identity. */
export function formatUserAgent(identity) {
	const mode = identity?.userAgent ?? "detailed";
	if (typeof mode === "object") return mode.custom;
	if (mode === "short") return "kiana";
	const product = `kiana/${getClientVersion(identity)}`;
	const runtime = runtimeDescription();
	return runtime
		? `${product} (${process.platform}; ${runtime}; ${process.arch})`
		: `${product} (browser)`;
}

const NVIDIA_NIM_HOST = "integrate.api.nvidia.com";
const OPENCODE_HOST = "opencode.ai";

function matchesHost(baseUrl, expectedHost) {
	try {
		return new URL(baseUrl).hostname === expectedHost;
	} catch {
		return false;
	}
}

function isOpenRouter(target) {
	return target.provider === "openrouter" || target.baseUrl.includes("openrouter.ai");
}

function isNvidiaNim(target) {
	return target.provider === "nvidia" || matchesHost(target.baseUrl, NVIDIA_NIM_HOST);
}

function isOpenCode(target) {
	return (
		target.provider === "opencode" ||
		target.provider === "opencode-go" ||
		matchesHost(target.baseUrl, OPENCODE_HOST)
	);
}

/** Provider attribution headers of an identity for a request target. */
export function getAttributionHeaders(target, identity) {
	const attribution = identity?.attribution;
	if (!attribution) return {};
	const { name, url, categories } = attribution;
	if (isOpenRouter(target)) {
		return {
			"HTTP-Referer": url,
			"X-OpenRouter-Title": name,
			...(categories === undefined || categories.length === 0
				? {}
				: { "X-OpenRouter-Categories": categories.join(",") }),
		};
	}
	if (isNvidiaNim(target)) return { "X-BILLING-INVOKE-ORIGIN": name.charAt(0).toUpperCase() + name.slice(1) };
	if (isOpenCode(target)) return { "x-opencode-client": name };
	return {};
}

/** OpenCode's per-conversation routing header, or undefined for other targets. */
export function getOpenCodeSessionHeaders(target, sessionId) {
	return sessionId && isOpenCode(target) ? { "x-opencode-session": sessionId } : undefined;
}

/** Default headers an identity adds to a request: `User-Agent` and provider attribution. */
export function getIdentityHeaders(target, identity) {
	return { "User-Agent": formatUserAgent(identity), ...getAttributionHeaders(target, identity) };
}

function isMutableModels(models) {
	return "setProvider" in models && "deleteProvider" in models && "clearProviders" in models;
}

/** `models` with `identity` as the default identity of its requests (TS `withClientIdentity`). */
export function withClientIdentity(models, identity) {
	const withIdentity = (options) => ({ ...options, identity: mergeClientIdentity(identity, options?.identity) });
	const wrapped = {
		getProviders: () => models.getProviders(),
		getProvider: (id) => models.getProvider(id),
		getModels: (provider) => models.getModels(provider),
		getModel: (provider, id) => models.getModel(provider, id),
		getModelsOfType: (type, provider) => models.getModelsOfType(type, provider),
		getModelOfType: (type, provider, id) => models.getModelOfType(type, provider, id),
		getAllModels: (provider) => models.getAllModels(provider),
		refresh: (options) => models.refresh(options),
		checkAuth: (providerId, options) => models.checkAuth(providerId, options),
		getAvailable: (providerId, options) => models.getAvailable(providerId, options),
		getAvailableOfType: (type, providerId, options) => models.getAvailableOfType(type, providerId, options),
		getAllAvailable: (providerId, options) => models.getAllAvailable(providerId, options),
		getAuth: (...args) => models.getAuth(...args),
		login: (providerId, type, interaction, options) => models.login(providerId, type, interaction, options),
		logout: (providerId, options) => models.logout(providerId, options),
		stream: (model, context, options) => models.stream(model, context, withIdentity(options)),
		complete: (model, context, options) => models.complete(model, context, withIdentity(options)),
		streamSimple: (model, context, options) => models.streamSimple(model, context, withIdentity(options)),
		completeSimple: (model, context, options) => models.completeSimple(model, context, withIdentity(options)),
		streamDeferred: (model, handle, options) => models.streamDeferred(model, handle, withIdentity(options)),
		fetchDeferred: (model, handle, options) => models.fetchDeferred(model, handle, withIdentity(options)),
		cancelDeferred: (model, handle, options) => models.cancelDeferred(model, handle, withIdentity(options)),
		generateImages: (model, context, options) => models.generateImages(model, context, withIdentity(options)),
		classify: (model, context, options) => models.classify(model, context, withIdentity(options)),
	};
	if (!isMutableModels(models)) return wrapped;
	return {
		...wrapped,
		setProvider: (provider) => models.setProvider(provider),
		deleteProvider: (id) => models.deleteProvider(id),
		clearProviders: () => models.clearProviders(),
	};
}

// ---------------------------------------------------------------------------
// utils/retry.ts
// ---------------------------------------------------------------------------

function buildProviderErrorPattern(patterns) {
	return new RegExp(patterns.join("|"), "i");
}

const NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN = buildProviderErrorPattern([
	"GoUsageLimitError",
	"FreeUsageLimitError",
	"Monthly usage limit reached",
	"available balance",
	"insufficient_quota",
	"out of budget",
	"quota exceeded",
	"billing",
	"subscription_sharing_usage_limit_exceeded",
	"subscription_sharing_workplace_individual_usage_limit_exceeded",
]);

const RETRYABLE_PROVIDER_ERROR_PATTERN = buildProviderErrorPattern([
	"overloaded",
	"server_busy",
	"servers are currently busy",
	"currently experiencing high demand",
	"model is at capacity",
	"rate.?limit",
	"too many requests",
	"(?<![a-z0-9])(?:429|500|502|503|504|520|524)(?![a-z0-9])",
	"service.?(?:is.?)?(?:temporarily.?)?unavailable",
	"server.?error",
	"internal.?error",
	"provider.?returned.?(?:an.?)?error",
	"exceeded request buffer limit while retrying upstream",
	"unable to forward upstream",
	"upstream.?request.?failed",
	"upstream_stream_read_error",
	"stream was interrupted",
	"http/?2 stream failed",
	"network.?error",
	"connection.?error",
	"connection.?refused",
	"connection.?lost",
	"other side closed",
	"fetch failed",
	"request to \\S+ failed, reason:",
	"getaddrinfo",
	"ENOTFOUND",
	"EAI_AGAIN",
	"upstream.?connect",
	"reset before headers",
	"socket hang up",
	"socket connection was closed",
	"timed? out",
	"timeout",
	"terminated",
	"websocket.?closed",
	"websocket.?error",
	"ended without",
	"stream ended before message_stop",
	"stream ended before a terminal response event",
	"http2 request did not get a response",
	"stream disconnected before completion",
	"pending stream has been canceled",
	"retry delay",
	"you can retry your request",
	"try your request again",
	"please retry your request",
	"ResourceExhausted",
	"subscription_sharing_usage_unavailable",
	"subscription_sharing_user_unavailable",
]);

/** `baseDelayMs * 2^(attempt-1)`, capped by `maxAgentDelayMs` (default 60s). */
export function retryDelayMs(policy, attempt) {
	const delay = policy.baseDelayMs * 2 ** Math.max(0, attempt - 1);
	const safeDelay = Number.isSafeInteger(delay) ? delay : Number.MAX_SAFE_INTEGER;
	return Math.min(safeDelay, policy.maxAgentDelayMs ?? DEFAULT_MAX_AGENT_RETRY_DELAY_MS);
}

class RetrySleepAbortError extends Error {
	constructor() {
		super("Aborted");
	}
}

function sleep(ms, signal) {
	return new Promise((resolve, reject) => {
		if (signal?.aborted) {
			reject(new RetrySleepAbortError());
			return;
		}
		const timeout = setTimeout(resolve, ms);
		signal?.addEventListener(
			"abort",
			() => {
				clearTimeout(timeout);
				reject(new RetrySleepAbortError());
			},
			{ once: true },
		);
	});
}

/** Transient provider/transport failure classifier (TS `isRetryableAssistantError`). */
export function isRetryableAssistantError(message) {
	if (message.stopReason !== "error" || !message.errorMessage) return false;
	const errorMessage = message.errorMessage;
	if (NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN.test(errorMessage)) return false;
	return RETRYABLE_PROVIDER_ERROR_PATTERN.test(errorMessage);
}

/** Run one assistant-producing call with bounded retry and exponential backoff. */
export async function retryAssistantCall(produce, policy, signal, callbacks) {
	const maxAttempts = policy?.enabled ? policy.maxRetries : 0;

	let attempt = 0;
	let lastRetry;
	for (;;) {
		const response = await produce();

		if (response.stopReason === "aborted") {
			if (lastRetry) await callbacks?.onRetryFinished?.(false, lastRetry.attempt);
			return response;
		}

		if (response.stopReason !== "error") {
			if (lastRetry) await callbacks?.onRetryFinished?.(true, lastRetry.attempt);
			return response;
		}

		if (attempt >= maxAttempts || !isRetryableAssistantError(response)) {
			if (lastRetry) await callbacks?.onRetryFinished?.(false, lastRetry.attempt, response.errorMessage);
			return response;
		}

		attempt++;
		lastRetry = { attempt, errorMessage: response.errorMessage || "Unknown error" };
		const delayMs = retryDelayMs(policy, attempt);
		await callbacks?.onRetryScheduled?.(attempt, maxAttempts, delayMs, lastRetry.errorMessage);

		try {
			await sleep(delayMs, signal);
		} catch (error) {
			await callbacks?.onRetryFinished?.(false, attempt, lastRetry.errorMessage);
			if (error instanceof RetrySleepAbortError) {
				const { errorMessage: _errorMessage, ...rest } = response;
				return { ...rest, stopReason: "aborted" };
			}
			throw error;
		}
		await callbacks?.onRetryAttemptStart?.();
	}
}

// ---------------------------------------------------------------------------
// api/lazy.ts + auth/helpers.ts
// ---------------------------------------------------------------------------

function createSetupErrorMessage(model, error, timestamp) {
	return {
		role: "assistant",
		content: [],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "error",
		errorMessage: error instanceof Error ? error.message : String(error),
		timestamp,
	};
}

function hasResult(source) {
	return typeof source.result === "function";
}

async function forwardStream(target, source, observer, observeResult) {
	let result;
	for await (const event of source) {
		target.push(event);
		observer?.onEvent?.(event);
		if (event.type === "done") result = event.message;
		else if (event.type === "error") result = event.error;
	}
	if (hasResult(source)) result = await source.result();
	target.end(result);
	observeResult(result);
}

/** Returns a stream synchronously while running async setup behind it. */
export function lazyStream(model, setup, observer) {
	const startedAt = Date.now();
	const outer = new AssistantMessageEventStream();
	let observedResult = false;
	const observeResult = (result) => {
		if (observedResult) return;
		observedResult = true;
		observer?.onResult?.(result);
	};

	setup()
		.then((inner) => forwardStream(outer, inner, observer, observeResult))
		.catch((error) => {
			const message = createSetupErrorMessage(model, error, startedAt);
			const event = { type: "error", reason: "error", error: message };
			outer.push(event);
			observer?.onEvent?.(event);
			outer.end(message);
			observeResult(message);
		});

	return outer;
}

/** Wrap a lazily loaded `ProviderStreams` module. */
export function lazyApi(load, capabilities) {
	const api = {
		stream: (model, context, options) =>
			lazyStream(model, async () => (await load()).stream(model, context, options)),
		streamSimple: (model, context, options) =>
			lazyStream(model, async () => (await load()).streamSimple(model, context, options)),
	};

	if (capabilities?.fetchDeferred) {
		api.fetchDeferred = (model, handle, options) =>
			lazyStream(model, async () => {
				const implementation = await load();
				if (!implementation.fetchDeferred) throw new Error("API does not support deferred responses");
				return implementation.fetchDeferred(model, handle, options);
			});
	}
	if (capabilities?.cancelDeferred) {
		api.cancelDeferred = async (model, handle, options) => {
			const implementation = await load();
			if (!implementation.cancelDeferred) throw new Error("API cannot cancel deferred responses");
			await implementation.cancelDeferred(model, handle, options);
		};
	}

	return api;
}

/** Wrap a dynamically imported `OAuthAuth` (loads on first use, then cached). */
export function lazyOAuth(input) {
	let promise;
	const loaded = () => {
		promise ??= input.load();
		return promise;
	};
	return {
		name: input.name,
		isSubscription: input.isSubscription,
		loginLabel: input.loginLabel,
		login: async (interaction, options) => (await loaded()).login(interaction, options),
		refresh: async (credential, signal) => (await loaded()).refresh(credential, signal),
		toAuth: async (credential) => (await loaded()).toAuth(credential),
	};
}

/** Standard api-key auth: stored credential key wins, otherwise the first set env var. */
export function envApiKeyAuth(name, envVars) {
	return {
		name,
		login: async (interaction) => {
			interaction.signal.throwIfAborted();
			const key = await interaction.prompt({ type: "secret", message: `Enter ${name}` });
			interaction.signal.throwIfAborted();
			return { type: "api_key", key };
		},
		resolve: async ({ ctx, credential, signal }) => {
			signal.throwIfAborted();
			if (credential?.key) {
				return { auth: { apiKey: credential.key }, env: credential.env, source: "stored credential" };
			}
			for (const envVar of envVars) {
				const value = await ctx.env(envVar);
				signal.throwIfAborted();
				if (value) return { auth: { apiKey: value }, source: envVar };
			}
			return undefined;
		},
	};
}

// ---------------------------------------------------------------------------
// images.ts + images-api-registry.ts
// ---------------------------------------------------------------------------

const imagesApiProviderRegistry = new Map();

/** Register an image API provider; `generateImages` is wrapped with its api check. */
export function registerImagesApiProvider(provider, sourceId) {
	const api = provider.api;
	imagesApiProviderRegistry.set(api, {
		provider: {
			api,
			generateImages: (model, context, options) => {
				if (model.api !== api) {
					throw new Error(`Mismatched api: ${model.api} expected ${api}`);
				}
				return provider.generateImages(model, context, options);
			},
		},
		sourceId,
	});
}

/** The registered image API provider for `api`, or undefined. */
export function getImagesApiProvider(api) {
	return imagesApiProviderRegistry.get(api)?.provider;
}

/** Global image generation dispatched on `model.api` through the images api registry. */
export async function generateImages(model, context, options) {
	const provider = getImagesApiProvider(model.api);
	if (!provider) {
		throw new Error(`No API provider registered for api: ${model.api}`);
	}
	return provider.generateImages(model, context, options);
}

// ---------------------------------------------------------------------------
// session-resources.ts
// ---------------------------------------------------------------------------

const sessionResourceCleanups = new Set();

/** Register a cleanup callback; the returned function unregisters it. */
export function registerSessionResourceCleanup(cleanup) {
	sessionResourceCleanups.add(cleanup);
	return () => {
		sessionResourceCleanups.delete(cleanup);
	};
}

/** Run every registered cleanup; failures are aggregated into one AggregateError. */
export function cleanupSessionResources(sessionId) {
	const errors = [];
	for (const cleanup of sessionResourceCleanups) {
		try {
			cleanup(sessionId);
		} catch (error) {
			errors.push(error);
		}
	}
	if (errors.length > 0) {
		throw new AggregateError(errors, "Failed to cleanup session resources");
	}
}

// ---------------------------------------------------------------------------
// api-registry + compat stream/complete (compat.ts)
// ---------------------------------------------------------------------------

const BUILTIN_APIS = [
	"anthropic-messages",
	"openai-completions",
	"openai-responses",
	"openai-codex-responses",
	"azure-openai-responses",
	"google-generative-ai",
	"google-vertex",
	"mistral-conversations",
	"bedrock-converse-stream",
	"kiana-messages",
];

const apiProviderRegistry = new Map();
const builtinApiProviderInstances = new Map();

/** Call a Rust-backed builtin stream. `mode` is `"stream"` or `"streamSimple"`. */
function builtinProviderStream(api, mode, model, context, options) {
	const stream = new AssistantMessageEventStream();
	__kiana
		.promise("kiana-ai.provider-stream", {
			api,
			mode,
			model,
			context,
			options,
			onEvent: (event) => stream.push(event),
		})
		.catch((error) => {
			stream.push({
				type: "error",
				error: {
					role: "assistant",
					timestamp: Date.now(),
					content: [],
					usage: {
						input: 0,
						output: 0,
						cacheRead: 0,
						cacheWrite: 0,
						totalTokens: 0,
						cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
					},
					stopReason: "error",
					errorMessage: error instanceof Error ? error.message : String(error),
				},
			});
		});
	return stream;
}

function makeBuiltinApiProvider(api) {
	return {
		api,
		stream: (model, context, options) => builtinProviderStream(api, "stream", model, context, options),
		streamSimple: (model, context, options) =>
			builtinProviderStream(api, "streamSimple", model, context, options),
	};
}

function wrapStream(api, stream) {
	return (model, context, options) => {
		if (model.api !== api) {
			throw new Error(`Mismatched api: ${model.api} expected ${api}`);
		}
		return stream(model, context, options);
	};
}

function wrapStreamSimple(api, streamSimple) {
	return (model, context, options) => {
		if (model.api !== api) {
			throw new Error(`Mismatched api: ${model.api} expected ${api}`);
		}
		return streamSimple(model, context, options);
	};
}

/** Register an API provider (later registrations win). */
export function registerApiProvider(provider, sourceId) {
	apiProviderRegistry.set(provider.api, {
		provider: {
			api: provider.api,
			stream: wrapStream(provider.api, provider.stream),
			streamSimple: wrapStreamSimple(provider.api, provider.streamSimple),
		},
		sourceId,
	});
}

/** Remove every provider registered under `sourceId`. */
export function unregisterApiProviders(sourceId) {
	for (const [api, entry] of [...apiProviderRegistry]) {
		if (entry.sourceId === sourceId) apiProviderRegistry.delete(api);
	}
}

/** Get one registered API provider. */
export function getApiProvider(api) {
	return apiProviderRegistry.get(api)?.provider;
}

/** All registered API providers. */
export function getApiProviders() {
	return [...apiProviderRegistry.values()].map((entry) => entry.provider);
}

/** Register the builtin API implementations without clobbering existing entries. */
export function registerBuiltInApiProviders() {
	for (const api of BUILTIN_APIS) {
		if (!getApiProvider(api)) registerApiProvider(makeBuiltinApiProvider(api));
		builtinApiProviderInstances.set(api, getApiProvider(api));
	}
}

/** Clear all providers and register the builtins again. */
export function resetApiProviders() {
	apiProviderRegistry.clear();
	builtinApiProviderInstances.clear();
	registerBuiltInApiProviders();
}

registerBuiltInApiProviders();

const AMBIENT_AUTH_MARKER = "<authenticated>";

function hasExplicitApiKey(apiKey) {
	return typeof apiKey === "string" && apiKey.trim().length > 0;
}

function withEnvApiKey(model, options) {
	if (hasExplicitApiKey(options?.apiKey)) return options;
	const namespace = globalThis.__kiana_virtual?.["@earendil-works/kiana-ai"];
	const getEnvApiKey = namespace?.getEnvApiKey;
	const apiKey = typeof getEnvApiKey === "function" ? getEnvApiKey(model.provider, options?.env) : undefined;
	if (!apiKey || apiKey === AMBIENT_AUTH_MARKER) return options;
	return { ...options, apiKey };
}

function hasResolvedCloudflareAuth(options) {
	return hasExplicitApiKey(options?.apiKey) || typeof options?.headers?.["cf-aig-authorization"] === "string";
}

function getBuiltinProviderForModel(model) {
	if (getApiProvider(model.api) !== builtinApiProviderInstances.get(model.api)) return undefined;
	const namespace = globalThis.__kiana_virtual?.["@earendil-works/kiana-ai/providers/all"];
	if (!namespace) return undefined;
	const providers = namespace.getBuiltinProviders?.() ?? [];
	if (!providers.includes(model.provider)) return undefined;
	const models = namespace.getBuiltinModels?.(model.provider) ?? [];
	return models.some((candidate) => candidate.api === model.api) ? model.provider : undefined;
}

function resolveApiProvider(api) {
	const provider = getApiProvider(api);
	if (!provider) throw new Error(`No API provider registered for api: ${api}`);
	return provider;
}

/** `stream(model, context, options?)`. */
export function stream(model, context, options) {
	const transcript = normalizeContext(context);
	const builtinProvider = getBuiltinProviderForModel(model);
	if (builtinProvider) {
		if (model.provider.startsWith("cloudflare-") && !hasResolvedCloudflareAuth(options)) {
			return builtinProviderStream(model.api, "stream", model, transcript, options);
		}
		return builtinApiProviderInstances
			.get(model.api)
			.stream(model, transcript, withEnvApiKey(model, options));
	}
	const provider = resolveApiProvider(model.api);
	return provider.stream(model, transcript, withEnvApiKey(model, options));
}

/** `streamSimple(model, context, options?)`. */
export function streamSimple(model, context, options) {
	const transcript = normalizeContext(context);
	const builtinProvider = getBuiltinProviderForModel(model);
	if (builtinProvider) {
		if (model.provider.startsWith("cloudflare-") && !hasResolvedCloudflareAuth(options)) {
			return builtinProviderStream(model.api, "streamSimple", model, transcript, options);
		}
		return builtinApiProviderInstances
			.get(model.api)
			.streamSimple(model, transcript, withEnvApiKey(model, options));
	}
	const provider = resolveApiProvider(model.api);
	return provider.streamSimple(model, transcript, withEnvApiKey(model, options));
}

/** `complete(model, context, options?)`. */
export async function complete(model, context, options) {
	return stream(model, context, options).result();
}

/** `completeSimple(model, context, options?)`. */
export async function completeSimple(model, context, options) {
	return streamSimple(model, context, options).result();
}

// Lazy per-API entrypoints (api/*.lazy.ts): each returns `{ stream, streamSimple }`.
function apiStreams(api) {
	return {
		stream: (model, context, options) => builtinProviderStream(api, "stream", model, context, options),
		streamSimple: (model, context, options) =>
			builtinProviderStream(api, "streamSimple", model, context, options),
	};
}

/** `anthropicMessagesApi()`. */
export function anthropicMessagesApi() {
	return apiStreams("anthropic-messages");
}
/** `azureOpenAIResponsesApi()`. */
export function azureOpenAIResponsesApi() {
	return apiStreams("azure-openai-responses");
}
/** `bedrockConverseStreamApi()`. */
export function bedrockConverseStreamApi() {
	return apiStreams("bedrock-converse-stream");
}
/** `googleGenerativeAIApi()`. */
export function googleGenerativeAIApi() {
	return apiStreams("google-generative-ai");
}
/** `googleVertexApi()`. */
export function googleVertexApi() {
	return apiStreams("google-vertex");
}
/** `mistralConversationsApi()`. */
export function mistralConversationsApi() {
	return apiStreams("mistral-conversations");
}
/** `openAICodexResponsesApi()`. */
export function openAICodexResponsesApi() {
	return apiStreams("openai-codex-responses");
}
/** `openAICompletionsApi()`. */
export function openAICompletionsApi() {
	return apiStreams("openai-completions");
}
/** `openAIResponsesApi()`. */
export function openAIResponsesApi() {
	return apiStreams("openai-responses");
}
/** `kianaMessagesApi()`. */
export function kianaMessagesApi() {
	return apiStreams("kiana-messages");
}

/** `setBedrockProviderModule()`. */
export function setBedrockProviderModule() {
	// The Rust port embeds the bedrock provider; the TS setter has no runtime effect here.
}

// legacy-api-aliases.ts: the same lazy streams, exported under the old names.
export function streamAnthropic(model, context, options) {
	return apiStreams("anthropic-messages").stream(model, context, options);
}
export function streamSimpleAnthropic(model, context, options) {
	return apiStreams("anthropic-messages").streamSimple(model, context, options);
}
export function streamAzureOpenAIResponses(model, context, options) {
	return apiStreams("azure-openai-responses").stream(model, context, options);
}
export function streamSimpleAzureOpenAIResponses(model, context, options) {
	return apiStreams("azure-openai-responses").streamSimple(model, context, options);
}
export function streamGoogle(model, context, options) {
	return apiStreams("google-generative-ai").stream(model, context, options);
}
export function streamSimpleGoogle(model, context, options) {
	return apiStreams("google-generative-ai").streamSimple(model, context, options);
}
export function streamGoogleVertex(model, context, options) {
	return apiStreams("google-vertex").stream(model, context, options);
}
export function streamSimpleGoogleVertex(model, context, options) {
	return apiStreams("google-vertex").streamSimple(model, context, options);
}
export function streamMistral(model, context, options) {
	return apiStreams("mistral-conversations").stream(model, context, options);
}
export function streamSimpleMistral(model, context, options) {
	return apiStreams("mistral-conversations").streamSimple(model, context, options);
}
export function streamOpenAICodexResponses(model, context, options) {
	return apiStreams("openai-codex-responses").stream(model, context, options);
}
export function streamSimpleOpenAICodexResponses(model, context, options) {
	return apiStreams("openai-codex-responses").streamSimple(model, context, options);
}
export function streamOpenAICompletions(model, context, options) {
	return apiStreams("openai-completions").stream(model, context, options);
}
export function streamSimpleOpenAICompletions(model, context, options) {
	return apiStreams("openai-completions").streamSimple(model, context, options);
}
export function streamOpenAIResponses(model, context, options) {
	return apiStreams("openai-responses").stream(model, context, options);
}
export function streamSimpleOpenAIResponses(model, context, options) {
	return apiStreams("openai-responses").streamSimple(model, context, options);
}

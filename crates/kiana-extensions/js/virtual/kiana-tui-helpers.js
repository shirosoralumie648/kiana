// Pure-JS values of the `@earendil-works/kiana-tui` virtual namespace (design §5.1, §8.3).
//
// Evaluated once by `virtual_modules::install`; the exported values are copied onto the
// `__kiana_virtual["@earendil-works/kiana-tui"]` namespace object and `js/virtual/kiana-tui.js` re-exports
// them from there. Native classes and functions are NOT defined here: the ui-adapter unit installs
// them on the native bridge (`globalThis.__kiana_native["kiana-tui"]`), which the namespace forwards to.
//
// Ports, verbatim in behaviour:
//   - packages/tui/src/tui.ts            -> Container, dispatchMouseEvent, CURSOR_MARKER,
//                                           FAKE_CURSOR_START/END, renderFakeCursor
//   - packages/tui/src/components/stack.ts, v-stack.ts, h-stack.ts -> Stack/VStack/HStack,
//                                           allocateStackSizes, visibleStackEntries
//   - `Marked` is the design §8.3 stub (marked is not embedded).

// ---------------------------------------------------------------------------
// Cursor markers (tui.ts)
// ---------------------------------------------------------------------------

export const CURSOR_MARKER = "\x1b_kiana:c\x07";
export const FAKE_CURSOR_START = "\x1b_kiana:fc\x07";
export const FAKE_CURSOR_END = "\x1b_kiana:/fc\x07";

/** Wrap text in fake cursor markers. */
export function renderFakeCursor(text) {
	return `${FAKE_CURSOR_START}${text}${FAKE_CURSOR_END}`;
}

/**
 * OSC 8 hyperlink (terminal-image.ts:745-747), verbatim.
 *
 * In-repo extensions import it (`.kiana/extensions/prompt-url-widget.ts`); the bytes must match the
 * TS implementation exactly because they are written to the terminal.
 */
export function hyperlink(text, url) {
	return `\x1b]8;;${url}\x1b\\${text}\x1b]8;;\x1b\\`;
}

// ---------------------------------------------------------------------------
// Mouse dispatch (tui.ts)
// ---------------------------------------------------------------------------

/** Dispatch a mouse event to one component, honouring forwarding results. */
export function dispatchMouseEvent(component, event) {
	const result = component.handleMouse?.(event);
	if (!result) return undefined;
	if ("target" in result) {
		// The component forwarded the event to a child it hosts. Like a delegating container, it
		// routes keys to that child itself, so it keeps keyboard focus.
		const forwarded = result;
		return forwarded.focus && component.handleInput
			? { ...forwarded, focusTarget: component }
			: forwarded;
	}
	if (!result.handled && !result.capture && !result.focus) return undefined;
	return {
		...result,
		handled: true,
		...(result.focus ? { focusTarget: component } : {}),
		target: {
			component,
			originX: event.screenX - event.x,
			originY: event.screenY - event.y,
			width: event.width,
			height: event.height,
		},
	};
}

// ---------------------------------------------------------------------------
// Container (tui.ts:358-429)
// ---------------------------------------------------------------------------

/** Container - a component that contains other components. */
export class Container {
	children = [];
	mouseLayout;

	addChild(component) {
		this.children.push(component);
	}

	removeChild(component) {
		const index = this.children.indexOf(component);
		if (index !== -1) {
			this.children.splice(index, 1);
		}
	}

	clear() {
		this.children = [];
	}

	invalidate() {
		for (const child of this.children) {
			child.invalidate?.();
		}
	}

	handleMouse(event) {
		if (event.y < 0 || event.y >= event.height) return undefined;
		const mouseChildren =
			this.mouseLayout?.width === event.width
				? this.mouseLayout.children
				: this.children.map((component) => ({
						component,
						height: component.render(event.width).length,
					}));
		let childY = 0;
		for (const { component: child, height: childHeight } of mouseChildren) {
			if (event.y >= childY && event.y < childY + childHeight) {
				const result = dispatchMouseEvent(child, {
					...event,
					y: event.y - childY,
					height: childHeight,
				});
				if (result?.focus && this.handleInput) return { ...result, focusTarget: this };
				return result;
			}
			childY += childHeight;
		}
		return undefined;
	}

	render(width) {
		const lines = [];
		const mouseChildren = [];
		for (const child of this.children) {
			const childLines = child.render(width);
			mouseChildren.push({ component: child, height: childLines.length });
			for (const line of childLines) {
				lines.push(line);
			}
		}
		this.mouseLayout = { width, children: mouseChildren };
		return lines;
	}
}

// ---------------------------------------------------------------------------
// Stack / VStack / HStack (components/stack.ts, v-stack.ts, h-stack.ts)
// ---------------------------------------------------------------------------

export const LAYOUT_NODE = Symbol.for("@earendil-works/kiana-tui/layout-node");

function isStackEntry(child) {
	return !("render" in child);
}

function normalizeSize(value, fallback) {
	return value === undefined || !Number.isFinite(value) ? fallback : Math.max(0, Math.floor(value));
}

export class Stack extends Container {
	entries = [];
	gap;
	align;

	constructor(children = [], options = {}) {
		super();
		this.gap = normalizeSize(options.gap, 0);
		this.align = options.align ?? "stretch";
		for (const child of children) {
			if (isStackEntry(child)) this.addChild(child.component, child);
			else this.addChild(child);
		}
	}

	addChild(component, options = {}) {
		super.addChild(component);
		this.entries.push({
			component,
			...(options.basis === undefined ? {} : { basis: options.basis }),
			...(options.grow === undefined ? {} : { grow: normalizeSize(options.grow, 0) }),
			...(options.shrink === undefined ? {} : { shrink: normalizeSize(options.shrink, 1) }),
			...(options.minSize === undefined
				? {}
				: { minSize: normalizeSize(options.minSize, 0) }),
			...(options.maxSize === undefined
				? {}
				: { maxSize: normalizeSize(options.maxSize, Number.MAX_SAFE_INTEGER) }),
			...(options.visible === undefined ? {} : { visible: options.visible }),
		});
	}

	removeChild(component) {
		super.removeChild(component);
		const index = this.entries.findIndex((entry) => entry.component === component);
		if (index !== -1) this.entries.splice(index, 1);
	}

	clear() {
		super.clear();
		this.entries.length = 0;
	}

	[LAYOUT_NODE]() {
		return {
			type: this.layoutType,
			entries: this.entries,
			gap: this.gap,
			align: this.align,
		};
	}
}

export function visibleStackEntries(entries, viewport) {
	return entries.filter((entry) => entry.visible?.(viewport) ?? true);
}

function clampSize(size, entry) {
	const min = Math.max(0, Math.floor(entry.minSize ?? 0));
	const max = Math.max(min, Math.floor(entry.maxSize ?? Number.MAX_SAFE_INTEGER));
	return Math.max(min, Math.min(max, Math.max(0, Math.floor(size))));
}

function distribute(sizes, entries, amount, mode) {
	let remaining = amount;
	while (remaining > 0) {
		const candidates = entries
			.map((entry, index) => ({ entry, index }))
			.filter(({ entry, index }) => {
				if (mode === "grow") {
					return (
						(entry.grow ?? 0) > 0 &&
						sizes[index] < (entry.maxSize ?? Number.MAX_SAFE_INTEGER)
					);
				}
				return (entry.shrink ?? 1) > 0 && sizes[index] > (entry.minSize ?? 0);
			});
		if (candidates.length === 0) return;

		const totalWeight = candidates.reduce((sum, { entry, index }) => {
			return (
				sum +
				(mode === "grow" ? (entry.grow ?? 0) : (entry.shrink ?? 1) * Math.max(1, sizes[index]))
			);
		}, 0);
		let distributed = 0;
		for (const { entry, index } of candidates) {
			if (remaining <= 0) break;
			const weight =
				mode === "grow" ? (entry.grow ?? 0) : (entry.shrink ?? 1) * Math.max(1, sizes[index]);
			const proposed = Math.max(1, Math.floor((remaining * weight) / totalWeight));
			const capacity =
				mode === "grow"
					? (entry.maxSize ?? Number.MAX_SAFE_INTEGER) - sizes[index]
					: sizes[index] - (entry.minSize ?? 0);
			const delta = Math.min(remaining, proposed, capacity);
			if (delta <= 0) continue;
			sizes[index] = sizes[index] + (mode === "grow" ? delta : -delta);
			remaining -= delta;
			distributed += delta;
		}
		if (distributed === 0) return;
	}
}

export function allocateStackSizes(entries, intrinsicSizes, availableSize, gap) {
	const sizes = entries.map((entry, index) =>
		clampSize(
			entry.basis === undefined || entry.basis === "auto"
				? (intrinsicSizes[index] ?? 0)
				: entry.basis,
			entry,
		),
	);
	if (availableSize === undefined) return sizes;

	const contentSize = Math.max(
		0,
		Math.floor(availableSize) - Math.max(0, entries.length - 1) * gap,
	);
	const total = sizes.reduce((sum, size) => sum + size, 0);
	if (total < contentSize) distribute(sizes, entries, contentSize - total, "grow");
	else if (total > contentSize) distribute(sizes, entries, total - contentSize, "shrink");
	return sizes;
}

/** Read a native kiana-tui member through the virtual namespace (bridge installed later). */
function tuiNative(name) {
	const namespace = globalThis.__kiana_virtual?.["@earendil-works/kiana-tui"];
	const value = namespace?.[name];
	if (value === undefined) {
		throw new Error(
			`kiana: native bridge member "kiana-tui.${name}" is not installed (ui-adapter unit)`,
		);
	}
	return value;
}

export class VStack extends Stack {
	layoutType = "vstack";

	constructor(children = [], options = {}) {
		super(children, options);
	}

	render(width) {
		const viewport = { width: Math.max(1, width), height: Number.MAX_SAFE_INTEGER };
		const entries = visibleStackEntries(this.entries, viewport);
		const rendered = entries.map((entry) => entry.component.render(viewport.width));
		const sizes = allocateStackSizes(
			entries,
			rendered.map((lines) => lines.length),
			undefined,
			this.gap,
		);
		const lines = [];
		for (let index = 0; index < entries.length; index++) {
			if (index > 0) {
				for (let gap = 0; gap < this.gap; gap++) lines.push("");
			}
			const childLines = rendered[index].slice(0, sizes[index]);
			lines.push(...childLines);
			for (let padding = childLines.length; padding < sizes[index]; padding++) lines.push("");
		}
		return lines;
	}
}

export class HStack extends Stack {
	layoutType = "hstack";

	constructor(children = [], options = {}) {
		super(children, options);
	}

	render(width) {
		const safeWidth = Math.max(1, width);
		const visibleWidth = tuiNative("visibleWidth");
		const compositeTuiLine = tuiNative("compositeTuiLine");
		const viewport = { width: safeWidth, height: Number.MAX_SAFE_INTEGER };
		const entries = visibleStackEntries(this.entries, viewport);
		if (entries.length === 0) return [];

		const intrinsicWidths = entries.map((entry) => {
			const lines = entry.component.render(safeWidth);
			return lines.reduce((max, line) => Math.max(max, visibleWidth(line)), 0);
		});
		const widths = allocateStackSizes(entries, intrinsicWidths, safeWidth, this.gap);
		const rendered = entries.map((entry, index) =>
			widths[index] === 0 ? [] : entry.component.render(widths[index]),
		);
		const height = rendered.reduce((max, lines) => Math.max(max, lines.length), 0);
		const result = Array.from({ length: height }, () => "");
		let x = 0;
		for (let index = 0; index < rendered.length; index++) {
			const lines = rendered[index];
			const childWidth = widths[index];
			let offset = 0;
			if (this.align === "center") offset = Math.floor((height - lines.length) / 2);
			else if (this.align === "end") offset = height - lines.length;
			for (let row = 0; row < lines.length; row++) {
				const target = row + offset;
				if (target < 0 || target >= result.length) continue;
				result[target] = compositeTuiLine(result[target], lines[row], x, childWidth, safeWidth);
			}
			x += childWidth + this.gap;
		}
		return result;
	}
}

// ---------------------------------------------------------------------------
// marked stub (design §8.3)
// ---------------------------------------------------------------------------

/** `marked` is not embedded in kiana; constructing it reports that instead of yielding undefined. */
export class Marked {
	constructor() {
		throw new Error("marked is not bundled in kiana");
	}
}

// Namespace builder for the kiana-* virtual module seeds (design §3.3).
//
// Evaluated once by `virtual_modules::install`. `makeNamespace(spec, values)` returns the object
// stored in `globalThis.__kiana_virtual[<public key>]`:
//
//   - `values` are pure-JS exports (StringEnum, Container, …) and Rust-provided constants, copied
//     as data properties;
//   - `spec.bridge` members come from `globalThis.__kiana_native[<namespace>]` at USE time:
//       * "callable" -> a stable Proxy that forwards apply/construct/get,
//       * "object"   -> a stable Proxy that forwards get/ownKeys/has,
//       * "primitive"-> an enumerable getter returning the bridge value.
//
// `spec.bridge` entries are `[name, kind, namespace, bridgeName?]`. `namespace` is the canonical
// bridge id for the member, which is not always the namespace being built: the TypeScript compat
// entrypoint re-exports the models module, so `kiana-ai.calculateCost ===
// kiana-ai/models.calculateCost`. `bridgeName` is the name in that bridge section when the TS export
// is an alias (`compat.getModel === providersAll.getBuiltinModel`); it defaults to `name`.
// Proxies are memoized per `(namespace, bridgeName, kind)`, so both namespace seeds receive the
// SAME JS object.
//
// The bridge is installed by other units after virtual_modules (ui-adapter, kiana-ai bridge, …), so
// forwarding is dynamic. A member that is used before its bridge exists throws a clear error;
// merely reading it (`typeof`, `Object.keys`) does not. The proxies are stable, so
// `export const X = ns.X` in the shim modules keeps working after the bridge lands.

/** Bridge namespaces that may provide a member of a `kiana-ai*` namespace (shared re-exports). */
function siblingNamespaces(namespace) {
	return namespace === "kiana-ai" || namespace.startsWith("kiana-ai/")
		? ["kiana-ai", "kiana-ai/models", "kiana-ai/oauth", "kiana-ai/providers-all"]
		: [namespace];
}

function lookup(namespace, name) {
	const bridge = globalThis.__kiana_native;
	for (const candidate of siblingNamespaces(namespace)) {
		const target = bridge === undefined || bridge === null ? undefined : bridge[candidate];
		if (target !== undefined && target !== null && name in target) {
			const value = target[name];
			if (value !== undefined) return value;
		}
	}
	throw new Error(`kiana: native bridge member "${namespace}.${name}" is not installed`);
}

function callableProxy(namespace, name) {
	return new Proxy(function () {}, {
		apply(_target, thisArg, args) {
			return Reflect.apply(lookup(namespace, name), thisArg, args);
		},
		construct(_target, args, newTarget) {
			return Reflect.construct(lookup(namespace, name), args, newTarget);
		},
		get(_target, property) {
			const value = lookup(namespace, name);
			if (property === "prototype") return value.prototype;
			return Reflect.get(value, property, value);
		},
		set(_target, property, newValue) {
			return Reflect.set(lookup(namespace, name), property, newValue);
		},
		has(_target, property) {
			return Reflect.has(lookup(namespace, name), property);
		},
		ownKeys() {
			return Reflect.ownKeys(lookup(namespace, name));
		},
		getOwnPropertyDescriptor(_target, property) {
			return Reflect.getOwnPropertyDescriptor(lookup(namespace, name), property);
		},
	});
}

function objectProxy(namespace, name) {
	return new Proxy(
		{},
		{
			get(_target, property) {
				const value = lookup(namespace, name);
				return Reflect.get(value, property, value);
			},
			set(_target, property, newValue) {
				return Reflect.set(lookup(namespace, name), property, newValue);
			},
			has(_target, property) {
				return Reflect.has(lookup(namespace, name), property);
			},
			ownKeys() {
				return Reflect.ownKeys(lookup(namespace, name));
			},
			getOwnPropertyDescriptor(_target, property) {
				return Reflect.getOwnPropertyDescriptor(lookup(namespace, name), property);
			},
		},
	);
}

/** Stable proxy per `(namespace, name, kind)`; shared by every seed that re-exports the member. */
const memberCache = new Map();

function bridgeMember(namespace, name, kind) {
	const key = `${namespace}\u0000${name}\u0000${kind}`;
	let member = memberCache.get(key);
	if (member === undefined) {
		member = kind === "object" ? objectProxy(namespace, name) : callableProxy(namespace, name);
		memberCache.set(key, member);
	}
	return member;
}

/** Build one namespace seed. `spec.bridge` entries select the native bridge section. */
export function makeNamespace(spec, values) {
	const target = {};
	for (const name of Object.keys(values)) target[name] = values[name];
	for (const [name, kind, namespace, bridgeName = name] of spec.bridge) {
		if (kind === "primitive") {
			Object.defineProperty(target, name, {
				enumerable: true,
				configurable: true,
				get() {
					return lookup(namespace, bridgeName);
				},
			});
		} else if (kind === "object") {
			target[name] = bridgeMember(namespace, bridgeName, "object");
		} else {
			target[name] = bridgeMember(namespace, bridgeName, "callable");
		}
	}
	return target;
}

/**
 * Define a non-enumerable alias on an object (the virtual-module globals).
 *
 * Canonical module names are registered this way: the CJS loader receives the resolver's
 * canonical name (`kiana-virtual/kiana-ai.js`, `typebox/build/index.mjs`) and looks it up with
 * `hasOwnProperty`, while the public 22-key object must keep exactly those enumerable keys.
 */
export function defineHidden(target, name, value) {
	Object.defineProperty(target, name, {
		value,
		enumerable: false,
		configurable: true,
		writable: false,
	});
}

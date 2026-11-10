// @earendil-works/kiana-ai/oauth | @mariozechner/kiana-ai/oauth (design §3.3).
//
// The OAuth entrypoint is a type-only compatibility barrel: it has no runtime exports.

const ns = globalThis.__kiana_virtual['@earendil-works/kiana-ai/oauth'];


// TS has no `default` export; jiti's interop hands the namespace object to
// `import x from "..."` (verified with jiti 2.7 in upstream-reference). QuickJS requires a real
// export, so the seed object itself is the default.
export default ns;

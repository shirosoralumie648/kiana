// @earendil-works/kiana-agent-core | @mariozechner/kiana-agent-core (design §8.4).
//
// Every import in the corpus is `import type`, so the runtime namespace is empty; the key has to
// exist so a value import can still resolve.
const ns = globalThis.__kiana_virtual["@earendil-works/kiana-agent-core"];

// jiti's interop default for a module without exports is the (empty) namespace object.
export default ns;

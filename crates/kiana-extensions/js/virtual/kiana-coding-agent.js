// @earendil-works/kiana-coding-agent | @mariozechner/kiana-coding-agent (design §8.6, 34 exports).
//
// Both keys alias this module name (one namespace object). `defineTool` and the four constants
// are values of the module itself; everything else is backed by the coding-agent bridge.

const ns = globalThis.__kiana_virtual['@earendil-works/kiana-coding-agent'];

export const BorderedLoader = ns.BorderedLoader;
export const CONFIG_DIR_NAME = ns.CONFIG_DIR_NAME;
export const CustomEditor = ns.CustomEditor;
export const DEFAULT_MAX_BYTES = ns.DEFAULT_MAX_BYTES;
export const DEFAULT_MAX_LINES = ns.DEFAULT_MAX_LINES;
export const DynamicBorder = ns.DynamicBorder;
export const VERSION = ns.VERSION;
export const convertToLlm = ns.convertToLlm;
export const createBashTool = ns.createBashTool;
export const createBashToolDefinition = ns.createBashToolDefinition;
export const createEditTool = ns.createEditTool;
export const createEditToolDefinition = ns.createEditToolDefinition;
export const createFindTool = ns.createFindTool;
export const createFindToolDefinition = ns.createFindToolDefinition;
export const createGrepTool = ns.createGrepTool;
export const createGrepToolDefinition = ns.createGrepToolDefinition;
export const createLsTool = ns.createLsTool;
export const createLsToolDefinition = ns.createLsToolDefinition;
export const createReadTool = ns.createReadTool;
export const createReadToolDefinition = ns.createReadToolDefinition;
export const createWriteTool = ns.createWriteTool;
export const createWriteToolDefinition = ns.createWriteToolDefinition;
export const defineTool = ns.defineTool;
export const formatReadLineRange = ns.formatReadLineRange;
export const formatSize = ns.formatSize;
export const getAgentDir = ns.getAgentDir;
export const getMarkdownTheme = ns.getMarkdownTheme;
export const getSettingsListTheme = ns.getSettingsListTheme;
export const keyHint = ns.keyHint;
export const parseFrontmatter = ns.parseFrontmatter;
export const serializeConversation = ns.serializeConversation;
export const truncateHead = ns.truncateHead;
export const truncateLine = ns.truncateLine;
export const withFileMutationQueue = ns.withFileMutationQueue;

// TS has no `default` export; jiti's interop hands the namespace object to
// `import x from "..."` (verified with jiti 2.7 in upstream-reference). QuickJS requires a real
// export, so the seed object itself is the default.
export default ns;

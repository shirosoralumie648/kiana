// @earendil-works/kiana-tui | @mariozechner/kiana-tui (design §5.1, §8.3).
//
// Both keys alias this module name (one namespace object). The export list is the complete runtime
// surface of `packages/tui/src/index.ts` plus the six coding-agent helpers the design exposes from
// this namespace; JS composition classes/constants come from kiana-tui-helpers.js, native
// classes/functions are forwarded to the ui-adapter bridge.

const ns = globalThis.__kiana_virtual['@earendil-works/kiana-tui'];

export const BorderedLoader = ns.BorderedLoader;
export const Box = ns.Box;
export const CURSOR_MARKER = ns.CURSOR_MARKER;
export const CancellableLoader = ns.CancellableLoader;
export const CombinedAutocompleteProvider = ns.CombinedAutocompleteProvider;
export const Container = ns.Container;
export const CustomEditor = ns.CustomEditor;
export const DynamicBorder = ns.DynamicBorder;
export const Editor = ns.Editor;
export const HStack = ns.HStack;
export const Image = ns.Image;
export const Input = ns.Input;
export const Key = ns.Key;
export const KeybindingsManager = ns.KeybindingsManager;
export const Loader = ns.Loader;
export const Markdown = ns.Markdown;
export const Marked = ns.Marked;
export const MouseRegion = ns.MouseRegion;
export const ProcessTerminal = ns.ProcessTerminal;
export const ScrollView = ns.ScrollView;
export const SelectList = ns.SelectList;
export const SettingsList = ns.SettingsList;
export const Spacer = ns.Spacer;
export const StdinBuffer = ns.StdinBuffer;
export const TUI_KEYBINDINGS = ns.TUI_KEYBINDINGS;
export const Text = ns.Text;
export const TruncatedText = ns.TruncatedText;
export const TuiAltScreen = ns.TuiAltScreen;
export const TuiMainScreen = ns.TuiMainScreen;
export const VStack = ns.VStack;
export const allocateImageId = ns.allocateImageId;
export const backgroundAnsi = ns.backgroundAnsi;
export const calculateImageRows = ns.calculateImageRows;
export const colorToHex = ns.colorToHex;
export const colorToOkhsl = ns.colorToOkhsl;
export const colorToOklch = ns.colorToOklch;
export const colorToRgb = ns.colorToRgb;
export const compositeTuiLine = ns.compositeTuiLine;
export const decodeKittyPrintable = ns.decodeKittyPrintable;
export const deleteAllKittyImages = ns.deleteAllKittyImages;
export const deleteKittyImage = ns.deleteKittyImage;
export const detectCapabilities = ns.detectCapabilities;
export const encodeITerm2 = ns.encodeITerm2;
export const encodeKitty = ns.encodeKitty;
export const foregroundAnsi = ns.foregroundAnsi;
export const formatProgramStatus = ns.formatProgramStatus;
export const fuzzyFilter = ns.fuzzyFilter;
export const fuzzyMatch = ns.fuzzyMatch;
export const getCapabilities = ns.getCapabilities;
export const getCellDimensions = ns.getCellDimensions;
export const getGifDimensions = ns.getGifDimensions;
export const getImageDimensions = ns.getImageDimensions;
export const getJpegDimensions = ns.getJpegDimensions;
export const getKeybindings = ns.getKeybindings;
export const getMarkdownTheme = ns.getMarkdownTheme;
export const getNativeClipboard = ns.getNativeClipboard;
export const getOsc8LinkAtColumn = ns.getOsc8LinkAtColumn;
export const getPngDimensions = ns.getPngDimensions;
export const getSettingsListTheme = ns.getSettingsListTheme;
export const getTerminalColorMode = ns.getTerminalColorMode;
export const getWebpDimensions = ns.getWebpDimensions;
export const hyperlink = ns.hyperlink;
export const imageFallback = ns.imageFallback;
export const indexedColor = ns.indexedColor;
export const isAppleTerminalSession = ns.isAppleTerminalSession;
export const isFocusable = ns.isFocusable;
export const isKeyRelease = ns.isKeyRelease;
export const isKeyRepeat = ns.isKeyRepeat;
export const isKittyProtocolActive = ns.isKittyProtocolActive;
export const isViewportTUI = ns.isViewportTUI;
export const keyHint = ns.keyHint;
export const matchesKey = ns.matchesKey;
export const mixColors = ns.mixColors;
export const okhslColor = ns.okhslColor;
export const oklabToOkhslLightness = ns.oklabToOkhslLightness;
export const oklchColor = ns.oklchColor;
export const parseColor = ns.parseColor;
export const parseKey = ns.parseKey;
export const parseTerminalColorSchemeReport = ns.parseTerminalColorSchemeReport;
export const renderFakeCursor = ns.renderFakeCursor;
export const renderImage = ns.renderImage;
export const renderLatex = ns.renderLatex;
export const resetCapabilitiesCache = ns.resetCapabilitiesCache;
export const rgbColor = ns.rgbColor;
export const setCapabilities = ns.setCapabilities;
export const setCapabilityOverrides = ns.setCapabilityOverrides;
export const setCellDimensions = ns.setCellDimensions;
export const setImageTranscoder = ns.setImageTranscoder;
export const setKeybindings = ns.setKeybindings;
export const setKittyProtocolActive = ns.setKittyProtocolActive;
export const sliceByColumn = ns.sliceByColumn;
export const stripTerminalSequences = ns.stripTerminalSequences;
export const styleText = ns.styleText;
export const styleTextWithAnsi = ns.styleTextWithAnsi;
export const truncateToWidth = ns.truncateToWidth;
export const visibleWidth = ns.visibleWidth;
export const wrapTextWithAnsi = ns.wrapTextWithAnsi;

// TS has no `default` export; jiti's interop hands the namespace object to
// `import x from "..."` (verified with jiti 2.7 in upstream-reference). QuickJS requires a real
// export, so the seed object itself is the default.
export default ns;

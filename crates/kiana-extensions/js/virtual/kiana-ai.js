// @earendil-works/kiana-ai | /compat | @mariozechner/kiana-ai | /compat (design §3.3, §8.2).
//
// The TypeScript map resolves the root and `/compat` to the compat entrypoint (a strict superset),
// and both scopes to the same object. Here every key aliases this one module name, so QuickJS
// hands back a single namespace object for all four specifiers.
//
// The export list is the complete runtime surface of `packages/ai/src/compat.ts` (138 names,
// enumerated from the real module; `tests/fixtures/virtual/exports.json`). Pure-JS values come
// from kiana-ai-helpers.js, the rest is forwarded to the kiana-ai bridge.

const ns = globalThis.__kiana_virtual['@earendil-works/kiana-ai'];

export const AI_TELEMETRY_SCHEMA = ns.AI_TELEMETRY_SCHEMA;
export const ANTHROPIC_API_KEY_ENV = ns.ANTHROPIC_API_KEY_ENV;
export const ANTHROPIC_AUTH_TOKEN_ENV = ns.ANTHROPIC_AUTH_TOKEN_ENV;
export const ANTHROPIC_FEDERATION_RULE_ID_ENV = ns.ANTHROPIC_FEDERATION_RULE_ID_ENV;
export const ANTHROPIC_IDENTITY_TOKEN_FILE_ENV = ns.ANTHROPIC_IDENTITY_TOKEN_FILE_ENV;
export const ANTHROPIC_OAUTH_TOKEN_ENV = ns.ANTHROPIC_OAUTH_TOKEN_ENV;
export const ANTHROPIC_ORGANIZATION_ID_ENV = ns.ANTHROPIC_ORGANIZATION_ID_ENV;
export const ANTHROPIC_SERVICE_ACCOUNT_ID_ENV = ns.ANTHROPIC_SERVICE_ACCOUNT_ID_ENV;
export const ANTHROPIC_WORKSPACE_ID_ENV = ns.ANTHROPIC_WORKSPACE_ID_ENV;
export const AssistantMessageEventStream = ns.AssistantMessageEventStream;
export const DEFAULT_MAX_AGENT_RETRY_DELAY_MS = ns.DEFAULT_MAX_AGENT_RETRY_DELAY_MS;
export const EventStream = ns.EventStream;
export const InMemoryCredentialStore = ns.InMemoryCredentialStore;
export const InMemoryModelsStore = ns.InMemoryModelsStore;
export const ModelsError = ns.ModelsError;
export const KIANA_AI_VERSION = ns.KIANA_AI_VERSION;
export const KIANA_ATTRIBUTION = ns.KIANA_ATTRIBUTION;
export const StreamingJsonParser = ns.StreamingJsonParser;
export const StringEnum = ns.StringEnum;
export const ToolCallArgumentsStream = ns.ToolCallArgumentsStream;
export const Type = ns.Type;
export const anthropicMessagesApi = ns.anthropicMessagesApi;
export const appendAssistantMessageDiagnostic = ns.appendAssistantMessageDiagnostic;
export const azureOpenAIResponsesApi = ns.azureOpenAIResponsesApi;
export const bedrockConverseStreamApi = ns.bedrockConverseStreamApi;
export const calculateCost = ns.calculateCost;
export const clampThinkingLevel = ns.clampThinkingLevel;
export const cleanupSessionResources = ns.cleanupSessionResources;
export const collapseSystemMessages = ns.collapseSystemMessages;
export const complete = ns.complete;
export const completeSimple = ns.completeSimple;
export const contentText = ns.contentText;
export const createAssistantMessageDiagnostic = ns.createAssistantMessageDiagnostic;
export const createAssistantMessageEventStream = ns.createAssistantMessageEventStream;
export const createFauxCore = ns.createFauxCore;
export const createInitialSystemMessage = ns.createInitialSystemMessage;
export const createModels = ns.createModels;
export const createProvider = ns.createProvider;
export const declarationsEqual = ns.declarationsEqual;
export const defaultProviderAuthContext = ns.defaultProviderAuthContext;
export const envApiKeyAuth = ns.envApiKeyAuth;
export const extractDiagnosticError = ns.extractDiagnosticError;
export const fauxAssistantMessage = ns.fauxAssistantMessage;
export const fauxProvider = ns.fauxProvider;
export const fauxText = ns.fauxText;
export const fauxThinking = ns.fauxThinking;
export const fauxToolCall = ns.fauxToolCall;
export const findEnvKeys = ns.findEnvKeys;
export const formatThrownValue = ns.formatThrownValue;
export const formatUserAgent = ns.formatUserAgent;
export const generateImages = ns.generateImages;
export const generateImagesOpenRouter = ns.generateImagesOpenRouter;
export const getApiProvider = ns.getApiProvider;
export const getApiProviders = ns.getApiProviders;
export const getAttributionHeaders = ns.getAttributionHeaders;
export const getClientVersion = ns.getClientVersion;
export const getCurrentSystemMessage = ns.getCurrentSystemMessage;
export const getCurrentSystemPrompt = ns.getCurrentSystemPrompt;
export const getCurrentTools = ns.getCurrentTools;
export const getDeclaredTools = ns.getDeclaredTools;
export const getEnvApiKey = ns.getEnvApiKey;
export const getIdentityHeaders = ns.getIdentityHeaders;
export const getImageModel = ns.getImageModel;
export const getImageModels = ns.getImageModels;
export const getImageProviders = ns.getImageProviders;
export const getImagesApiProvider = ns.getImagesApiProvider;
export const getInitialSystemMessage = ns.getInitialSystemMessage;
export const getModel = ns.getModel;
export const getModelType = ns.getModelType;
export const getModels = ns.getModels;
export const getOpenCodeSessionHeaders = ns.getOpenCodeSessionHeaders;
export const getOverflowPatterns = ns.getOverflowPatterns;
export const getProviders = ns.getProviders;
export const getSupportedThinkingLevels = ns.getSupportedThinkingLevels;
export const getSystemMessageText = ns.getSystemMessageText;
export const getToolStateChanges = ns.getToolStateChanges;
export const googleGenerativeAIApi = ns.googleGenerativeAIApi;
export const googleVertexApi = ns.googleVertexApi;
export const hasApi = ns.hasApi;
export const hasNonAdditiveToolChanges = ns.hasNonAdditiveToolChanges;
export const hasToolRedefinitions = ns.hasToolRedefinitions;
export const isContextOverflow = ns.isContextOverflow;
export const isModelType = ns.isModelType;
export const isRecoverableLength = ns.isRecoverableLength;
export const isRetryableAssistantError = ns.isRetryableAssistantError;
export const lazyApi = ns.lazyApi;
export const lazyOAuth = ns.lazyOAuth;
export const lazyStream = ns.lazyStream;
export const mergeClientIdentity = ns.mergeClientIdentity;
export const mistralConversationsApi = ns.mistralConversationsApi;
export const modelsAreEqual = ns.modelsAreEqual;
export const normalizeContext = ns.normalizeContext;
export const openAICodexResponsesApi = ns.openAICodexResponsesApi;
export const openAICompletionsApi = ns.openAICompletionsApi;
export const openAIResponsesApi = ns.openAIResponsesApi;
export const parseJsonWithRepair = ns.parseJsonWithRepair;
export const parseStreamingJson = ns.parseStreamingJson;
export const parseToolCallArguments = ns.parseToolCallArguments;
export const kianaMessagesApi = ns.kianaMessagesApi;
export const registerApiProvider = ns.registerApiProvider;
export const registerBuiltInApiProviders = ns.registerBuiltInApiProviders;
export const registerBuiltInImagesApiProviders = ns.registerBuiltInImagesApiProviders;
export const registerFauxProvider = ns.registerFauxProvider;
export const registerImagesApiProvider = ns.registerImagesApiProvider;
export const registerSessionResourceCleanup = ns.registerSessionResourceCleanup;
export const renderSystemMessageUpdate = ns.renderSystemMessageUpdate;
export const repairJson = ns.repairJson;
export const resetApiProviders = ns.resetApiProviders;
export const resolveTranscript = ns.resolveTranscript;
export const resolveTranscriptTools = ns.resolveTranscriptTools;
export const retryAssistantCall = ns.retryAssistantCall;
export const retryDelayMs = ns.retryDelayMs;
export const setBedrockProviderModule = ns.setBedrockProviderModule;
export const stream = ns.stream;
export const streamAnthropic = ns.streamAnthropic;
export const streamAzureOpenAIResponses = ns.streamAzureOpenAIResponses;
export const streamGoogle = ns.streamGoogle;
export const streamGoogleVertex = ns.streamGoogleVertex;
export const streamMistral = ns.streamMistral;
export const streamOpenAICodexResponses = ns.streamOpenAICodexResponses;
export const streamOpenAICompletions = ns.streamOpenAICompletions;
export const streamOpenAIResponses = ns.streamOpenAIResponses;
export const streamSimple = ns.streamSimple;
export const streamSimpleAnthropic = ns.streamSimpleAnthropic;
export const streamSimpleAzureOpenAIResponses = ns.streamSimpleAzureOpenAIResponses;
export const streamSimpleGoogle = ns.streamSimpleGoogle;
export const streamSimpleGoogleVertex = ns.streamSimpleGoogleVertex;
export const streamSimpleMistral = ns.streamSimpleMistral;
export const streamSimpleOpenAICodexResponses = ns.streamSimpleOpenAICodexResponses;
export const streamSimpleOpenAICompletions = ns.streamSimpleOpenAICompletions;
export const streamSimpleOpenAIResponses = ns.streamSimpleOpenAIResponses;
export const toToolDeclaration = ns.toToolDeclaration;
export const unregisterApiProviders = ns.unregisterApiProviders;
export const uuidv7 = ns.uuidv7;
export const validateToolArguments = ns.validateToolArguments;
export const validateToolCall = ns.validateToolCall;
export const withClientIdentity = ns.withClientIdentity;
export const withoutInitialSystemMessage = ns.withoutInitialSystemMessage;

// TS has no `default` export; jiti's interop hands the namespace object to
// `import x from "..."` (verified with jiti 2.7 in upstream-reference). QuickJS requires a real
// export, so the seed object itself is the default.
export default ns;

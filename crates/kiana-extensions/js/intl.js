// Intl + toLocale* overrides (spec/design-extension-runtime.md §5.8, §6.4).
//
// Installed by `JsEngine`'s crate facade BEFORE every other module (§9.2 step
// 3): a module-level `new Intl.Segmenter()` must see this object. Formatting is
// delegated to Rust (`__kiana_host.intlNumber` / `intlDate` / `segment`), which
// uses ICU4X when the `locale-data` feature is enabled and a hand-written en-US
// path otherwise. `Intl.Collator`/`PluralRules`/`RelativeTimeFormat` are not
// supported and throw RangeError (design §6.4).
(function () {
  "use strict";

  var host = globalThis.__kiana_host;

  // ---------------------------------------------------------------------
  // Locale tags (Node/ECMA-402 validation)
  // ---------------------------------------------------------------------

  function canonicalizeTag(input) {
    if (typeof input !== "string") {
      throw new TypeError("Language ID should be string or object.");
    }
    var text = input;
    if (text.length === 0 || text.indexOf("_") !== -1 ||
        !/^[A-Za-z]{2,8}(-[A-Za-z0-9]{1,8})*$/.test(text)) {
      throw new RangeError("Invalid language tag: " + text);
    }
    var parts = text.split("-");
    var out = [parts[0].toLowerCase()];
    for (var i = 1; i < parts.length; i++) {
      var part = parts[i];
      if (part.length === 1) {
        // Extension singleton: the whole extension stays lowercase.
        out.push(part.toLowerCase());
        for (var j = i + 1; j < parts.length; j++) out.push(parts[j].toLowerCase());
        break;
      }
      if (part.length === 2 && /^[A-Za-z]+$/.test(part)) out.push(part.toUpperCase());
      else if (part.length === 3 && /^[0-9]+$/.test(part)) out.push(part);
      else if (part.length === 4 && /^[A-Za-z]+$/.test(part)) {
        out.push(part.charAt(0).toUpperCase() + part.slice(1).toLowerCase());
      } else {
        out.push(part.toLowerCase());
      }
    }
    return out.join("-");
  }

  function localeList(locales) {
    var out = [];
    if (locales === undefined || locales === null) return out;
    if (typeof locales === "string") {
      out.push(canonicalizeTag(locales));
      return out;
    }
    if (typeof locales === "object" && typeof locales.length === "number") {
      for (var i = 0; i < locales.length; i++) out.push(canonicalizeTag(locales[i]));
      return out;
    }
    throw new TypeError("Language ID should be string or object.");
  }

  function defaultLocale() {
    try {
      return canonicalizeTag(host.locale() || "en-US");
    } catch (_) {
      return "en-US";
    }
  }

  var SUPPORTED_LANGUAGES = Object.create(null);
  ["en", "de", "fr", "ru", "sv", "fi", "da", "nb", "nn", "no", "es", "it", "pt", "nl",
   "pl", "cs", "sk", "sl", "hr", "sr", "uk", "bg", "ro", "hu", "el", "tr", "ar", "he",
   "fa", "hi", "bn", "ta", "te", "th", "vi", "id", "ms", "ja", "ko", "zh", "ca", "lt",
   "lv", "et", "is", "sw", "am", "mr", "gu", "kn", "ml", "pa", "ur", "tl", "af", "sq",
   "hy", "az", "be", "bs", "eu", "fil", "ga", "gl", "ka", "kk", "km", "ky", "lo", "mk",
   "mn", "my", "ne", "si", "uz", "cy", "zu"].forEach(function (code) {
    SUPPORTED_LANGUAGES[code] = true;
  });

  function isSupportedTag(tag) {
    var language = tag.split("-")[0].toLowerCase();
    return SUPPORTED_LANGUAGES[language] === true;
  }

  function supportedLocales(locales) {
    var list = localeList(locales);
    var out = [];
    for (var i = 0; i < list.length; i++) {
      if (isSupportedTag(list[i]) && out.indexOf(list[i]) === -1) out.push(list[i]);
    }
    return out;
  }

  function normalizeLocale(locales) {
    var list = localeList(locales);
    for (var i = 0; i < list.length; i++) {
      if (isSupportedTag(list[i])) return list[i];
    }
    return defaultLocale();
  }

  function normalizedOptions(options) {
    if (options === undefined || options === null) return {};
    if (typeof options !== "object") return {};
    var copy = {};
    var keys = Object.keys(options);
    for (var i = 0; i < keys.length; i++) {
      if (options[keys[i]] !== undefined) copy[keys[i]] = options[keys[i]];
    }
    return copy;
  }

  function optionString(options, key) {
    return options[key] === undefined ? undefined : String(options[key]);
  }

  function checkEnum(value, key, allowed, namespace) {
    if (value === undefined) return;
    if (allowed.indexOf(value) === -1) {
      throw new RangeError(
        "Value " + value + " out of range for Intl." + namespace + " options property " + key,
      );
    }
  }

  var IntlObject = globalThis.Intl || {};
  Object.defineProperty(globalThis, "Intl", {
    value: IntlObject,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // ---------------------------------------------------------------------
  // Segmenter
  // ---------------------------------------------------------------------

  class Segmenter {
    constructor(locales, options) {
      this._locale = normalizeLocale(locales);
      options = normalizedOptions(options);
      this._granularity = options.granularity === undefined ? "grapheme" : String(options.granularity);
      if (["grapheme", "word", "sentence"].indexOf(this._granularity) === -1) {
        throw new RangeError("Invalid granularity: " + options.granularity);
      }
    }
    resolvedOptions() {
      return { locale: this._locale, granularity: this._granularity };
    }
    segment(input) {
      var text = String(input);
      var raw = JSON.parse(host.segment(text, this._granularity));
      var segments = [];
      for (var i = 0; i < raw.length; i++) {
        var entry = {
          segment: raw[i].segment,
          index: raw[i].index,
          input: text,
        };
        if (raw[i].isWordLike !== undefined) entry.isWordLike = raw[i].isWordLike;
        segments.push(entry);
      }
      return {
        [Symbol.iterator]: function () {
          var index = 0;
          return {
            next: function () {
              if (index >= segments.length) return { value: undefined, done: true };
              return { value: segments[index++], done: false };
            },
          };
        },
        containing: function (position) {
          position = position | 0;
          for (var i = 0; i < segments.length; i++) {
            var start = segments[i].index;
            var end = start + segments[i].segment.length;
            if (position >= start && position < end) return segments[i];
          }
          return undefined;
        },
      };
    }
    static supportedLocalesOf(locales) {
      return supportedLocales(locales);
    }
  }
  Segmenter.supportedLocalesOf = Segmenter.supportedLocalesOf;

  // ---------------------------------------------------------------------
  // NumberFormat
  // ---------------------------------------------------------------------

  var NUMBER_STYLES = ["decimal", "percent", "currency", "unit"];
  var NOTATIONS = ["standard", "scientific", "engineering", "compact"];
  var SIGN_DISPLAYS = ["auto", "never", "always", "exceptZero", "negative", "negativeZero"];
  var USE_GROUPINGS = ["auto", "always", "min2"];

  function currencyFractionDigits(code) {
    if (!code) return 2;
    if (["BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "PYG", "RWF", "UGX",
         "UYI", "VND", "VUV", "XAF", "XOF", "XPF"].indexOf(code) !== -1) return 0;
    if (["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"].indexOf(code) !== -1) return 3;
    return 2;
  }

  function defaultDigits(style, notation, currency) {
    if (style === "currency") {
      var digits = currencyFractionDigits(currency);
      return { minimum: digits, maximum: digits };
    }
    if (style === "percent") return { minimum: 0, maximum: 0 };
    if (notation === "compact") return { minimum: 0, maximum: undefined };
    return { minimum: 0, maximum: 3 };
  }

  class NumberFormat {
    constructor(locales, options) {
      this._locale = normalizeLocale(locales);
      this._options = normalizedOptions(options);
      var style = optionString(this._options, "style");
      if (style === undefined) style = "decimal";
      checkEnum(style, "style", NUMBER_STYLES, "NumberFormat");
      this._style = style;
      if (style === "currency") {
        if (this._options.currency === undefined || this._options.currency === "") {
          throw new TypeError("Currency code is required with currency style.");
        }
        var code = String(this._options.currency).toUpperCase();
        if (!/^[A-Z]{3}$/.test(code)) {
          throw new RangeError("Invalid currency code : " + this._options.currency);
        }
        this._options.currency = code;
      }
      if (style === "unit") {
        if (this._options.unit === undefined || this._options.unit === "") {
          throw new TypeError("Unit is required with unit style.");
        }
        this._options.unit = String(this._options.unit);
      }
      checkEnum(optionString(this._options, "currencyDisplay"), "currencyDisplay",
        ["code", "symbol", "narrowSymbol", "name"], "NumberFormat");
      checkEnum(optionString(this._options, "currencySign"), "currencySign",
        ["standard", "accounting"], "NumberFormat");
      checkEnum(optionString(this._options, "unitDisplay"), "unitDisplay",
        ["short", "narrow", "long"], "NumberFormat");
      checkEnum(optionString(this._options, "notation"), "notation", NOTATIONS, "NumberFormat");
      checkEnum(optionString(this._options, "signDisplay"), "signDisplay", SIGN_DISPLAYS, "NumberFormat");
      var grouping = this._options.useGrouping;
      if (grouping !== undefined && typeof grouping !== "boolean") {
        var groupingText = String(grouping);
        if (USE_GROUPINGS.indexOf(groupingText) === -1) {
          throw new RangeError(
            "Value " + groupingText +
            " out of range for Intl.NumberFormat options property useGrouping",
          );
        }
      } else if (grouping !== undefined) {
        this._options.useGrouping = grouping ? "always" : false;
      }
      var digits = defaultDigits(
        this._style,
        optionString(this._options, "notation") || "standard",
        this._options.currency,
      );
      if (this._options.minimumFractionDigits === undefined) {
        this._options.minimumFractionDigits = digits.minimum;
      }
      if (this._options.maximumFractionDigits === undefined && digits.maximum !== undefined) {
        this._options.maximumFractionDigits = digits.maximum;
      }
      this._options.minimumIntegerDigits = this._options.minimumIntegerDigits === undefined
        ? 1
        : Number(this._options.minimumIntegerDigits) | 0;
    }

    _parts(value) {
      if (typeof value === "bigint") {
        var bigResult = JSON.parse(
          host.intlNumberBig(this._locale, JSON.stringify(this._options), value.toString()),
        );
        return {
          text: bigResult.text,
          parts: bigResult.parts.map(function (entry) {
            return { type: entry[0], value: entry[1] };
          }),
        };
      }
      var number = Number(value);
      if (!isFinite(number)) {
        var text = number !== number ? "NaN" : number > 0 ? "∞" : "-∞";
        return { text: text, parts: [{ type: "nan", value: text }] };
      }
      var result = JSON.parse(
        host.intlNumber(this._locale, JSON.stringify(this._options), number),
      );
      var parts = [];
      for (var i = 0; i < result.parts.length; i++) {
        parts.push({ type: result.parts[i][0], value: result.parts[i][1] });
      }
      return { text: result.text, parts: parts };
    }

    // Node exposes `format`/`formatToParts` as bound accessors, so the
    // detached function keeps working.
    get format() {
      var self = this;
      return function (value) { return self._parts(value).text; };
    }

    get formatToParts() {
      var self = this;
      return function (value) { return self._parts(value).parts; };
    }

    resolvedOptions() {
      var grouping = this._options.useGrouping;
      var useGrouping = grouping === undefined ? "auto" : grouping;
      var options = this._options;
      var resolved = {
        locale: this._locale,
        numberingSystem: "latn",
        style: this._style,
      };
      if (this._style === "currency") {
        resolved.currency = options.currency;
        resolved.currencyDisplay = optionString(options, "currencyDisplay") || "symbol";
        resolved.currencySign = optionString(options, "currencySign") || "standard";
      }
      if (this._style === "unit") {
        resolved.unit = options.unit;
        resolved.unitDisplay = optionString(options, "unitDisplay") || "short";
      }
      resolved.minimumIntegerDigits = options.minimumIntegerDigits;
      resolved.minimumFractionDigits = options.minimumFractionDigits === undefined
        ? 0
        : options.minimumFractionDigits;
      resolved.maximumFractionDigits = options.maximumFractionDigits === undefined
        ? (optionString(options, "notation") === "compact" ? 1 : 3)
        : options.maximumFractionDigits;
      resolved.useGrouping = useGrouping;
      resolved.notation = optionString(options, "notation") || "standard";
      resolved.signDisplay = optionString(options, "signDisplay") || "auto";
      resolved.roundingIncrement = 1;
      resolved.roundingMode = "halfExpand";
      resolved.roundingPriority = "auto";
      resolved.trailingZeroDisplay = "auto";
      return resolved;
    }

    static supportedLocalesOf(locales) {
      return supportedLocales(locales);
    }
  }

  // ---------------------------------------------------------------------
  // DateTimeFormat
  // ---------------------------------------------------------------------

  var DATE_COMPONENTS = [
    "weekday", "era", "year", "month", "day", "dayPeriod",
    "hour", "minute", "second", "fractionalSecondDigits", "timeZoneName",
  ];

  var H23_LANGUAGES = [
    "de", "fr", "ru", "pl", "nl", "sv", "da", "fi", "cs", "es", "it", "pt",
    "tr", "uk", "hu", "ro", "el", "nb", "bg", "hr", "sk", "sl", "lt", "lv",
    "et", "is", "ca",
  ];

  function defaultHourCycle(locale) {
    var language = locale.split("-")[0].toLowerCase();
    return H23_LANGUAGES.indexOf(language) === -1 ? "h12" : "h23";
  }

  function hasComponent(options) {
    for (var i = 0; i < DATE_COMPONENTS.length; i++) {
      if (options[DATE_COMPONENTS[i]] !== undefined) return true;
    }
    return false;
  }

  function checkField(options, key, allowed) {
    var value = options[key];
    if (value === undefined) return undefined;
    value = String(value);
    checkEnum(value, key, allowed, "DateTimeFormat");
    return value;
  }

  function buildDateSpec(options, fallbackToDate) {
    var spec = {
      dateFields: null,
      length: "short",
      year2: false,
      pad: false,
      time: null,
      hourCycle: null,
      timeZone: null,
      padHour: false,
      minuteOnly: false,
      fractional: null,
      dayPeriod: null,
      zoneName: null,
      hasMinute: false,
      hasSecond: false,
    };
    if (options.hour12 !== undefined) {
      if (typeof options.hour12 !== "boolean") {
        throw new TypeError("Intl.DateTimeFormat options property hour12 must be a boolean");
      }
      spec.hourCycle = options.hour12 ? "h12" : "h23";
    }
    if (options.timeZone !== undefined) {
      var zone = String(options.timeZone);
      if (!host.intlValidTimeZone(zone)) {
        throw new RangeError("Invalid time zone specified: " + zone);
      }
      spec.timeZone = zone;
    }

    if (options.dateStyle !== undefined || options.timeStyle !== undefined) {
      // Node rejects mixing date/time styles with explicit components.
      if (hasComponent(options)) {
        throw new TypeError("Invalid option : option");
      }
      var dateStyle = checkField(options, "dateStyle", ["full", "long", "medium", "short"]);
      var timeStyle = checkField(options, "timeStyle", ["full", "long", "medium", "short"]);
      if (dateStyle !== undefined) {
        spec.dateStyle = dateStyle;
        if (dateStyle === "full") { spec.dateFields = "YMDE"; spec.length = "long"; }
        else if (dateStyle === "long") { spec.dateFields = "YMD"; spec.length = "long"; }
        else if (dateStyle === "medium") { spec.dateFields = "YMD"; spec.length = "medium"; }
        else { spec.dateFields = "YMD"; spec.length = "short"; spec.year2 = true; }
      }
      spec.styleJoin = (dateStyle === "full" || dateStyle === "long") &&
        timeStyle !== undefined ? "at" : null;
      if (timeStyle !== undefined) {
        spec.timeStyle = timeStyle;
        spec.time = timeStyle === "short" ? "minute" : "second";
        spec.hasMinute = true;
        spec.hasSecond = timeStyle !== "short";
        if (timeStyle === "full") spec.zoneName = "full";
        else if (timeStyle === "long") spec.zoneName = "short";
      }
      return spec;
    }

    var year = checkField(options, "year", ["numeric", "2-digit"]);
    var month = checkField(options, "month", ["numeric", "2-digit", "long", "short", "narrow"]);
    var day = checkField(options, "day", ["numeric", "2-digit"]);
    var weekday = checkField(options, "weekday", ["long", "short", "narrow"]);
    var hour = checkField(options, "hour", ["numeric", "2-digit"]);
    var minute = checkField(options, "minute", ["numeric", "2-digit"]);
    var second = checkField(options, "second", ["numeric", "2-digit"]);
    var era = checkField(options, "era", ["long", "short", "narrow"]);
    var timeZoneName = checkField(options, "timeZoneName",
      ["long", "short", "shortOffset", "longOffset", "shortGeneric", "longGeneric"]);
    var dayPeriod = checkField(options, "dayPeriod", ["long", "short", "narrow"]);
    var fractional = options.fractionalSecondDigits;
    if (fractional !== undefined) {
      fractional = Number(fractional);
      if (!(fractional === 1 || fractional === 2 || fractional === 3)) {
        throw new RangeError(
          "Value " + options.fractionalSecondDigits +
          " out of range for Intl.DateTimeFormat options property fractionalSecondDigits",
        );
      }
    }
    spec.padHour = hour === "2-digit";
    spec.hasMinute = minute !== undefined;
    spec.hasSecond = second !== undefined;
    spec.minuteOnly = hour === undefined && minute !== undefined && second === undefined &&
      fractional === undefined;
    spec.fractional = fractional === undefined ? null : fractional;
    spec.dayPeriod = dayPeriod === undefined ? null : dayPeriod;
    spec.zoneName = timeZoneName === undefined ? null : timeZoneName;

    if (year === undefined && month === undefined && day === undefined &&
        weekday === undefined && hour === undefined && minute === undefined &&
        second === undefined && era === undefined && timeZoneName === undefined) {
      if (fallbackToDate) {
        spec.dateFields = "YMD";
        spec.length = "short";
      }
      return spec;
    }

    if (year !== undefined || month !== undefined || day !== undefined ||
        weekday !== undefined || era !== undefined) {
      if (year !== undefined && month !== undefined && day !== undefined && weekday !== undefined) {
        spec.dateFields = "YMDE";
      } else if (year !== undefined && month !== undefined && day !== undefined) {
        spec.dateFields = "YMD";
      } else if (year !== undefined && month !== undefined) {
        spec.dateFields = "YM";
      } else if (month !== undefined && day !== undefined && weekday !== undefined) {
        spec.dateFields = "MDE";
      } else if (month !== undefined && day !== undefined) {
        spec.dateFields = "MD";
      } else if (day !== undefined && weekday !== undefined) {
        spec.dateFields = "DE";
      } else if (year !== undefined) {
        spec.dateFields = "Y";
      } else if (month !== undefined) {
        spec.dateFields = "M";
      } else if (day !== undefined) {
        spec.dateFields = "D";
      } else {
        spec.dateFields = "E";
      }
      if (month === "long" || (month === undefined && weekday === "long")) {
        spec.length = "long";
      } else if (month === "short" || month === "narrow" ||
                 (month === undefined && (weekday === "short" || weekday === "narrow"))) {
        spec.length = "medium";
      } else {
        spec.length = "short";
      }
      spec.year2 = year === "2-digit";
      spec.pad = month === "2-digit" || day === "2-digit";
    }

    if (hour !== undefined || minute !== undefined || second !== undefined ||
        fractional !== undefined) {
      spec.time = second !== undefined || fractional !== undefined
        ? "second"
        : minute !== undefined
          ? "minute"
          : "hour";
    }
    return spec;
  }

  class DateTimeFormat {
    constructor(locales, options) {
      this._locale = normalizeLocale(locales);
      this._options = normalizedOptions(options);
      this._spec = buildDateSpec(this._options, true);
    }
    resolvedOptions() {
      var options = this._options;
      var spec = this._spec;
      var resolved = {
        locale: this._locale,
        calendar: "gregory",
        numberingSystem: "latn",
        timeZone: spec.timeZone === null ? host.timeZone() : spec.timeZone,
      };
      if (spec.time !== null) {
        var cycle = this._hourCycle();
        resolved.hourCycle = cycle;
        resolved.hour12 = cycle !== "h23";
      }
      if (spec.dateStyle !== undefined) resolved.dateStyle = spec.dateStyle;
      if (spec.timeStyle !== undefined) resolved.timeStyle = spec.timeStyle;
      if (spec.dateStyle === undefined && spec.timeStyle === undefined) {
        var added = false;
        for (var i = 0; i < DATE_COMPONENTS.length; i++) {
          var key = DATE_COMPONENTS[i];
          if (options[key] !== undefined) {
            resolved[key] = String(options[key]);
            added = true;
          }
        }
        if (!added) {
          // Defaults were used (Intl.DateTimeFormat with no component options,
          // or the toLocale* default field set): report them like Node does.
          var fields = spec.dateFields || "";
          if (fields.indexOf("Y") !== -1) resolved.year = "numeric";
          if (fields.indexOf("M") !== -1) resolved.month = "numeric";
          if (fields.indexOf("D") !== -1) resolved.day = "numeric";
          if (fields === "E") resolved.weekday = "long";
          if (fields === "DE") { resolved.day = "numeric"; }
          if (fields === "MDE") { resolved.month = "numeric"; resolved.day = "numeric"; }
          if (spec.time !== null) {
            resolved.hour = "numeric";
            resolved.minute = "2-digit";
            resolved.second = "2-digit";
          }
        }
        if (resolved.hour !== undefined && (options.hour12 === false || this._hourCycle() === "h23")) {
          resolved.hour = "2-digit";
        }
        if (resolved.minute !== undefined) resolved.minute = "2-digit";
      }
      return resolved;
    }
    _hourCycle() {
      if (this._spec.hourCycle !== null) return this._spec.hourCycle;
      return defaultHourCycle(this._locale);
    }
    _output(date) {
      var value = date === undefined
        ? Date.now()
        : date instanceof Date ? date.getTime() : Number(date);
      if (!isFinite(value)) return null;
      return JSON.parse(host.intlDate(this._locale, JSON.stringify(this._spec), value));
    }
    _format(date) {
      var output = this._output(date);
      return output === null ? "Invalid Date" : output.text;
    }
    get format() {
      var self = this;
      return function (date) { return self._format(date); };
    }
    _formatToParts(date) {
      var output = this._output(date);
      if (output === null) return [{ type: "literal", value: "Invalid Date" }];
      return output.parts.map(function (entry) {
        return { type: entry[0], value: entry[1] };
      });
    }
    get formatToParts() {
      var self = this;
      return function (date) { return self._formatToParts(date); };
    }
    static supportedLocalesOf(locales) {
      return supportedLocales(locales);
    }
  }

  // ---------------------------------------------------------------------
  // Unsupported formatters
  // ---------------------------------------------------------------------

  function unsupportedClass(name) {
    return class {
      constructor() {
        throw new RangeError("Intl." + name + " is not supported");
      }
    };
  }

  /// ECMA-402 makes NumberFormat/DateTimeFormat callable without `new`; model
  /// that with a wrapper around the class (same prototype/statics).
  function callable(Class) {
    var wrapper = function (locales, options) {
      return new Class(locales, options);
    };
    wrapper.prototype = Class.prototype;
    Object.defineProperty(wrapper.prototype, "constructor", {
      value: wrapper,
      writable: true,
      configurable: true,
    });
    var keys = Object.getOwnPropertyNames(Class);
    for (var i = 0; i < keys.length; i++) {
      var key = keys[i];
      if (["length", "name", "prototype"].indexOf(key) !== -1) continue;
      Object.defineProperty(wrapper, key, Object.getOwnPropertyDescriptor(Class, key));
    }
    return wrapper;
  }

  IntlObject.Segmenter = Segmenter;
  IntlObject.NumberFormat = callable(NumberFormat);
  IntlObject.DateTimeFormat = callable(DateTimeFormat);
  IntlObject.Collator = unsupportedClass("Collator");
  IntlObject.PluralRules = unsupportedClass("PluralRules");
  IntlObject.RelativeTimeFormat = unsupportedClass("RelativeTimeFormat");
  IntlObject.ListFormat = unsupportedClass("ListFormat");
  IntlObject.DisplayNames = unsupportedClass("DisplayNames");
  IntlObject.getCanonicalLocales = function (locales) {
    return localeList(locales);
  };

  // ---------------------------------------------------------------------
  // toLocale* overrides (design §5.8/§6.4: writable:true, enumerable:false)
  // ---------------------------------------------------------------------

  function defineMethod(target, name, fn) {
    Object.defineProperty(target, name, {
      value: fn,
      writable: true,
      enumerable: false,
      configurable: true,
    });
  }

  /// Node semantics: the method defaults apply only when the caller supplied no
  /// date/time component (and no dateStyle/timeStyle).
  function localeDefaults(kind, options) {
    var user = normalizedOptions(options);
    if (user.dateStyle !== undefined || user.timeStyle !== undefined || hasComponent(user)) {
      return user;
    }
    var merged = {};
    for (var key in user) merged[key] = user[key];
    if (kind !== "time") {
      merged.year = "numeric";
      merged.month = "numeric";
      merged.day = "numeric";
    }
    if (kind !== "date") {
      merged.hour = "numeric";
      merged.minute = "numeric";
      merged.second = "numeric";
    }
    return merged;
  }

  defineMethod(Date.prototype, "toLocaleString", function (locales, options) {
    if (!isFinite(this.getTime())) return "Invalid Date";
    return new DateTimeFormat(locales, localeDefaults("both", options)).format(this);
  });
  defineMethod(Date.prototype, "toLocaleDateString", function (locales, options) {
    if (!isFinite(this.getTime())) return "Invalid Date";
    return new DateTimeFormat(locales, localeDefaults("date", options)).format(this);
  });
  defineMethod(Date.prototype, "toLocaleTimeString", function (locales, options) {
    if (!isFinite(this.getTime())) return "Invalid Date";
    return new DateTimeFormat(locales, localeDefaults("time", options)).format(this);
  });
  defineMethod(Number.prototype, "toLocaleString", function (locales, options) {
    return new NumberFormat(locales, options).format(this);
  });
  if (typeof BigInt !== "undefined" && BigInt.prototype) {
    defineMethod(BigInt.prototype, "toLocaleString", function (locales, options) {
      return new NumberFormat(locales, options).format(this);
    });
  }
})();

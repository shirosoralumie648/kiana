// node:path — POSIX and win32 semantics (design §6.1/§6.3).
//
// NOTE (reported deviation): ARCHITECTURE.md §2.2 forbids kiana-extensions from
// depending on kiana-durable, so GAPS G4.3's single node:path implementation in
// `kiana_durable::env::path` cannot be reused. This is the unit-local copy.
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var shims = globalThis.__kiana_node_shims;

  function cwd() {
    return I.host.cwd();
  }

  // ------------------------------ POSIX ------------------------------

  function posixNormalizeInternal(path, keepTrailing) {
    path = String(path);
    if (path.length === 0) return ".";
    var isAbsolute = path.charCodeAt(0) === 47;
    var trailing = keepTrailing && path.length > 1 && path.charAt(path.length - 1) === "/";
    var segments = path.split("/");
    var out = [];
    for (var i = 0; i < segments.length; i++) {
      var segment = segments[i];
      if (segment === "" || segment === ".") continue;
      if (segment === "..") {
        if (out.length > 0 && out[out.length - 1] !== "..") out.pop();
        else if (!isAbsolute) out.push("..");
      } else {
        out.push(segment);
      }
    }
    var result = out.join("/");
    var final;
    if (isAbsolute) final = "/" + result;
    else final = result;
    if (final === "") {
      final = isAbsolute ? "/" : trailing ? "./" : ".";
    } else if (trailing && final !== "/" && final.charAt(final.length - 1) !== "/") {
      final += "/";
    }
    return final;
  }

  function posixNormalize(path) {
    return posixNormalizeInternal(path, true);
  }

  function posixJoin() {
    var joined = "";
    for (var i = 0; i < arguments.length; i++) {
      var part = arguments[i];
      if (part === undefined) continue;
      part = String(part);
      if (part.length === 0) continue;
      if (joined.length > 0) joined += "/";
      joined += part;
    }
    if (joined.length === 0) return ".";
    return posixNormalizeInternal(joined, true);
  }

  function posixResolve() {
    var resolved = "";
    var isAbsolute = false;
    for (var i = arguments.length - 1; i >= 0; i--) {
      var part = arguments[i];
      if (part === undefined) continue;
      part = String(part);
      if (part.length === 0) continue;
      resolved = part + "/" + resolved;
      if (part.charAt(0) === "/") {
        isAbsolute = true;
        break;
      }
    }
    if (!isAbsolute) resolved = cwd() + "/" + resolved;
    return posixNormalizeInternal(resolved, false);
  }

  function posixDirname(path) {
    path = String(path);
    if (path.length === 0) return ".";
    var end = path.length - 1;
    while (end >= 0 && path.charAt(end) === "/") end--;
    if (end < 0) return "/";
    if (end === 0) return path.charAt(0) === "/" ? "/" : ".";
    var start = path.lastIndexOf("/", end) + 1;
    if (start === 0) return ".";
    var prefix = path.slice(0, start);
    if (prefix === "/") return "/";
    // POSIX exposes a leading "//" (implementation-defined root) verbatim.
    if (prefix === "//") return "//";
    return prefix.slice(0, -1);
  }

  function posixBasename(path, ext) {
    path = String(path);
    var end = path.length - 1;
    while (end >= 0 && path.charAt(end) === "/") end--;
    if (end < 0) return "";
    var start = path.lastIndexOf("/", end);
    var base = path.slice(start + 1, end + 1);
    if (ext && base.length > ext.length && base.slice(base.length - ext.length) === ext) {
      base = base.slice(0, base.length - ext.length);
    }
    return base;
  }

  function posixExtname(path) {
    var base = posixBasename(path);
    if (base.length === 0 || base === "." || base === "..") return "";
    var index = base.lastIndexOf(".");
    if (index <= 0) return "";
    return base.slice(index);
  }

  function posixIsAbsolute(path) {
    path = String(path);
    return path.charAt(0) === "/";
  }

  function posixRelative(from, to) {
    var fromResolved = posixResolve(from).split("/").filter(function (part) { return part !== ""; });
    var toResolved = posixResolve(to).split("/").filter(function (part) { return part !== ""; });
    var length = Math.min(fromResolved.length, toResolved.length);
    var index = 0;
    while (index < length && fromResolved[index] === toResolved[index]) index++;
    var parts = [];
    for (var i = index; i < fromResolved.length; i++) parts.push("..");
    for (var j = index; j < toResolved.length; j++) parts.push(toResolved[j]);
    return parts.join("/");
  }

  function posixParse(path) {
    path = String(path);
    var ret = { root: "", dir: "", base: "", ext: "", name: "" };
    if (path.length === 0) return ret;
    var isAbsolute = path.charCodeAt(0) === 47;
    var start;
    if (isAbsolute) {
      ret.root = "/";
      start = 1;
    } else {
      start = 0;
    }
    var startDot = -1;
    var startPart = 0;
    var end = -1;
    var matchedSlash = true;
    var i = path.length - 1;
    var preDotState = 0;
    for (; i >= start; --i) {
      var char = path.charAt(i);
      if (char === "/") {
        if (!matchedSlash) {
          startPart = i + 1;
          break;
        }
        continue;
      }
      if (end === -1) {
        matchedSlash = false;
        end = i + 1;
      }
      if (char === ".") {
        if (startDot === -1) startDot = i;
        else if (preDotState !== 1) preDotState = 1;
      } else if (startDot !== -1) {
        preDotState = -1;
      }
    }
    if (end !== -1) {
      var begin = startPart === 0 && isAbsolute ? 1 : startPart;
      if (startDot === -1 || preDotState === 0 ||
          (preDotState === 1 && startDot === end - 1 && startDot === startPart + 1)) {
        ret.base = ret.name = path.slice(begin, end);
      } else {
        ret.name = path.slice(begin, startDot);
        ret.base = path.slice(begin, end);
        ret.ext = path.slice(startDot, end);
      }
    }
    if (startPart > 0) ret.dir = path.slice(0, startPart - 1);
    else if (isAbsolute) ret.dir = "/";
    return ret;
  }

  function posixFormat(object) {
    var dir = object.dir || object.root || "";
    var base = object.base !== undefined
      ? object.base
      : (object.name || "") + (object.ext || "");
    if (!dir) return base;
    return dir === object.root ? dir + base : dir + "/" + base;
  }

  // ------------------------------ win32 ------------------------------
  //
  // 逐条移植 Node 26 `lib/path.js` 的 win32 实现（含 CVE-2024-36139 的
  // drive-relative 处理、保留设备名与 `\\?\` 设备根）。Node 的 win32 函数
  // **保留调用方传入的分隔符**（不把 `/` 改写成 `\\`），只有在需要比较或
  // 归一化时才把两种分隔符都当作分隔符。

  var CHAR_DOT = 46;
  var CHAR_FORWARD_SLASH = 47;
  var CHAR_COLON = 58;
  var CHAR_QUESTION_MARK = 63;
  var CHAR_BACKWARD_SLASH = 92;
  var CHAR_LOWERCASE_A = 97;
  var CHAR_LOWERCASE_Z = 122;
  var CHAR_UPPERCASE_A = 65;
  var CHAR_UPPERCASE_Z = 90;

  function isPathSeparator(code) {
    return code === CHAR_FORWARD_SLASH || code === CHAR_BACKWARD_SLASH;
  }

  function isPosixPathSeparator(code) {
    return code === CHAR_FORWARD_SLASH;
  }

  function isWindowsDeviceRoot(code) {
    return (code >= CHAR_UPPERCASE_A && code <= CHAR_UPPERCASE_Z) ||
      (code >= CHAR_LOWERCASE_A && code <= CHAR_LOWERCASE_Z);
  }

  var WINDOWS_RESERVED_NAMES = [
    "CON", "PRN", "AUX", "NUL",
    "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    "COM¹", "COM²", "COM³",
    "LPT¹", "LPT²", "LPT³",
  ];

  function isWindowsReservedName(path, colonIndex) {
    var devicePart = String(path).slice(0, colonIndex).toUpperCase();
    return WINDOWS_RESERVED_NAMES.indexOf(devicePart) !== -1;
  }

  /// Node 的 `normalizeString`：解析 `.`/`..`，保留大小写。
  function normalizeString(path, allowAboveRoot, separator, separatorCheck) {
    var res = "";
    var lastSegmentLength = 0;
    var lastSlash = -1;
    var dots = 0;
    var code = 0;
    for (var i = 0; i <= path.length; ++i) {
      if (i < path.length) code = path.charCodeAt(i);
      else if (separatorCheck(code)) break;
      else code = CHAR_FORWARD_SLASH;

      if (separatorCheck(code)) {
        if (lastSlash === i - 1 || dots === 1) {
          // NOOP
        } else if (dots === 2) {
          if (res.length < 2 || lastSegmentLength !== 2 ||
              res.charCodeAt(res.length - 1) !== CHAR_DOT ||
              res.charCodeAt(res.length - 2) !== CHAR_DOT) {
            if (res.length > 2) {
              var lastSlashIndex = res.length - lastSegmentLength - 1;
              if (lastSlashIndex === -1) {
                res = "";
                lastSegmentLength = 0;
              } else {
                res = res.slice(0, lastSlashIndex);
                lastSegmentLength = res.length - 1 - res.lastIndexOf(separator);
              }
              lastSlash = i;
              dots = 0;
              continue;
            } else if (res.length !== 0) {
              res = "";
              lastSegmentLength = 0;
              lastSlash = i;
              dots = 0;
              continue;
            }
          }
          if (allowAboveRoot) {
            res += res.length > 0 ? separator + ".." : "..";
            lastSegmentLength = 2;
          }
        } else {
          if (res.length > 0) res += separator + path.slice(lastSlash + 1, i);
          else res = path.slice(lastSlash + 1, i);
          lastSegmentLength = i - lastSlash - 1;
        }
        lastSlash = i;
        dots = 0;
      } else if (code === CHAR_DOT && dots !== -1) {
        ++dots;
      } else {
        dots = -1;
      }
    }
    return res;
  }

  function win32Resolve() {
    var args = Array.prototype.slice.call(arguments);
    var resolvedDevice = "";
    var resolvedTail = "";
    var resolvedAbsolute = false;

    for (var i = args.length - 1; i >= -1; i--) {
      var path;
      if (i >= 0) {
        path = String(args[i]);
        if (path.length === 0) continue;
      } else if (resolvedDevice.length === 0) {
        path = cwd();
        if (args.length === 0 ||
            (args.length === 1 && (String(args[0]) === "" || String(args[0]) === ".") &&
             isPathSeparator(path.charCodeAt(0)))) {
          // Node: on POSIX the cwd fast path rewrites `/` to `\\`.
          if (I.host.platform() === "win32") return path;
          return path.replace(/\//g, "\\");
        }
      } else {
        var driveCwd = undefined;
        try {
          if (typeof process !== "undefined" && process.env) {
            driveCwd = process.env["=" + resolvedDevice];
          }
        } catch (_) {}
        path = driveCwd || cwd();
        if (path === undefined ||
            (path.slice(0, 2).toLowerCase() !== resolvedDevice.toLowerCase() &&
             path.charCodeAt(2) === CHAR_BACKWARD_SLASH)) {
          path = resolvedDevice + "\\";
        }
      }

      var len = path.length;
      var rootEnd = 0;
      var device = "";
      var isAbsolute = false;
      var code = path.charCodeAt(0);

      if (len === 1) {
        if (isPathSeparator(code)) {
          rootEnd = 1;
          isAbsolute = true;
        }
      } else if (isPathSeparator(code)) {
        isAbsolute = true;
        if (isPathSeparator(path.charCodeAt(1))) {
          var j = 2;
          var last = j;
          while (j < len && !isPathSeparator(path.charCodeAt(j))) j++;
          if (j < len && j !== last) {
            var firstPart = path.slice(last, j);
            last = j;
            while (j < len && isPathSeparator(path.charCodeAt(j))) j++;
            if (j < len && j !== last) {
              last = j;
              while (j < len && !isPathSeparator(path.charCodeAt(j))) j++;
              if (j === len || j !== last) {
                if (firstPart !== "." && firstPart !== "?") {
                  device = "\\\\" + firstPart + "\\" + path.slice(last, j);
                  rootEnd = j;
                } else {
                  device = "\\\\" + firstPart;
                  rootEnd = 4;
                }
              }
            }
          }
        } else {
          rootEnd = 1;
        }
      } else if (isWindowsDeviceRoot(code) && path.charCodeAt(1) === CHAR_COLON) {
        device = path.slice(0, 2);
        rootEnd = 2;
        if (len > 2 && isPathSeparator(path.charCodeAt(2))) {
          isAbsolute = true;
          rootEnd = 3;
        }
      }

      if (device.length > 0) {
        if (resolvedDevice.length > 0) {
          if (device.toLowerCase() !== resolvedDevice.toLowerCase()) continue;
        } else {
          resolvedDevice = device;
        }
      }

      if (resolvedAbsolute) {
        if (resolvedDevice.length > 0) break;
      } else {
        resolvedTail = path.slice(rootEnd) + "\\" + resolvedTail;
        resolvedAbsolute = isAbsolute;
        if (isAbsolute && resolvedDevice.length > 0) break;
      }
    }

    resolvedTail = normalizeString(resolvedTail, !resolvedAbsolute, "\\", isPathSeparator);
    var result = resolvedAbsolute
      ? resolvedDevice + "\\" + resolvedTail
      : resolvedDevice + resolvedTail;
    return result || ".";
  }

  function win32Normalize(path) {
    path = String(path);
    var len = path.length;
    if (len === 0) return ".";
    var rootEnd = 0;
    var device;
    var isAbsolute = false;
    var code = path.charCodeAt(0);

    if (len === 1) {
      return isPosixPathSeparator(code) ? "\\" : path;
    }
    if (isPathSeparator(code)) {
      isAbsolute = true;
      if (isPathSeparator(path.charCodeAt(1))) {
        var j = 2;
        var last = j;
        while (j < len && !isPathSeparator(path.charCodeAt(j))) j++;
        if (j < len && j !== last) {
          var firstPart = path.slice(last, j);
          last = j;
          while (j < len && isPathSeparator(path.charCodeAt(j))) j++;
          if (j < len && j !== last) {
            last = j;
            while (j < len && !isPathSeparator(path.charCodeAt(j))) j++;
            if (j === len || j !== last) {
              if (firstPart === "." || firstPart === "?") {
                device = "\\\\" + firstPart;
                rootEnd = 4;
                var colonIndex = path.indexOf(":");
                var possibleDevice = path.slice(4, colonIndex + 1);
                if (isWindowsReservedName(possibleDevice, possibleDevice.length - 1)) {
                  device = "\\\\?\\" + possibleDevice;
                  rootEnd = 4 + possibleDevice.length;
                }
              } else if (j === len) {
                return "\\\\" + firstPart + "\\" + path.slice(last) + "\\";
              } else {
                device = "\\\\" + firstPart + "\\" + path.slice(last, j);
                rootEnd = j;
              }
            }
          }
        }
      } else {
        rootEnd = 1;
      }
    } else {
      var colon = path.indexOf(":");
      if (colon > 0) {
        if (isWindowsDeviceRoot(code) && colon === 1) {
          device = path.slice(0, 2);
          rootEnd = 2;
          if (len > 2 && isPathSeparator(path.charCodeAt(2))) {
            isAbsolute = true;
            rootEnd = 3;
          }
        } else if (isWindowsReservedName(path, colon)) {
          device = path.slice(0, colon + 1);
          rootEnd = colon + 1;
        }
      }
    }

    var tail = rootEnd < len
      ? normalizeString(path.slice(rootEnd), !isAbsolute, "\\", isPathSeparator)
      : "";
    if (tail.length === 0 && !isAbsolute) tail = ".";
    if (tail.length > 0 && isPathSeparator(path.charCodeAt(len - 1))) tail += "\\";
    if (!isAbsolute && device === undefined && path.indexOf(":") !== -1) {
      if (tail.length >= 2 && isWindowsDeviceRoot(tail.charCodeAt(0)) &&
          tail.charCodeAt(1) === CHAR_COLON) {
        return ".\\" + tail;
      }
      var index = path.indexOf(":");
      do {
        if (index === len - 1 || isPathSeparator(path.charCodeAt(index + 1))) {
          return ".\\" + tail;
        }
      } while ((index = path.indexOf(":", index + 1)) !== -1);
    }
    var reservedColon = path.indexOf(":");
    if (isWindowsReservedName(path, reservedColon)) {
      return ".\\" + (device === undefined ? "" : device) + tail;
    }
    if (device === undefined) {
      return isAbsolute ? "\\" + tail : tail;
    }
    return isAbsolute ? device + "\\" + tail : device + tail;
  }

  function win32IsAbsolute(path) {
    path = String(path);
    var len = path.length;
    if (len === 0) return false;
    var code = path.charCodeAt(0);
    return isPathSeparator(code) ||
      (len > 2 && isWindowsDeviceRoot(code) &&
       path.charCodeAt(1) === CHAR_COLON &&
       isPathSeparator(path.charCodeAt(2)));
  }

  function win32Join() {
    var args = Array.prototype.slice.call(arguments);
    if (args.length === 0) return ".";
    var path = [];
    for (var i = 0; i < args.length; ++i) {
      var arg = String(args[i]);
      if (arg.length > 0) path.push(arg);
    }
    if (path.length === 0) return ".";
    var firstPart = path[0];
    var joined = path.join("\\");

    var needsReplace = true;
    var slashCount = 0;
    if (isPathSeparator(firstPart.charCodeAt(0))) {
      ++slashCount;
      var firstLen = firstPart.length;
      if (firstLen > 1 && isPathSeparator(firstPart.charCodeAt(1))) {
        ++slashCount;
        if (firstLen > 2) {
          if (isPathSeparator(firstPart.charCodeAt(2))) ++slashCount;
          else needsReplace = false;
        }
      }
    }
    if (needsReplace) {
      while (slashCount < joined.length &&
             isPathSeparator(joined.charCodeAt(slashCount))) {
        slashCount++;
      }
      if (slashCount >= 2) joined = "\\" + joined.slice(slashCount);
    }

    var parts = [];
    var part = "";
    for (var j = 0; j < joined.length; j++) {
      if (joined[j] === "\\") {
        if (part) parts.push(part);
        part = "";
        while (j + 1 < joined.length && joined[j + 1] === "\\") j++;
      } else {
        part += joined[j];
      }
    }
    if (part) parts.push(part);
    var hasReserved = false;
    for (var k = 0; k < parts.length; k++) {
      var colonIndex = parts[k].indexOf(":");
      if (colonIndex !== -1 && isWindowsReservedName(parts[k], colonIndex)) {
        hasReserved = true;
        break;
      }
    }
    if (hasReserved) {
      var result = "";
      for (var m = 0; m < joined.length; m++) {
        result += joined[m] === "/" ? "\\" : joined[m];
      }
      return result;
    }
    return win32Normalize(joined);
  }

  function win32Relative(from, to) {
    from = String(from);
    to = String(to);
    if (from === to) return "";
    var fromOrig = win32Resolve(from);
    var toOrig = win32Resolve(to);
    if (fromOrig === toOrig) return "";

    from = fromOrig.toLowerCase();
    to = toOrig.toLowerCase();
    if (from === to) return "";

    if (fromOrig.length !== from.length || toOrig.length !== to.length) {
      var fromSplit = fromOrig.split("\\");
      var toSplit = toOrig.split("\\");
      if (fromSplit[fromSplit.length - 1] === "") fromSplit.pop();
      if (toSplit[toSplit.length - 1] === "") toSplit.pop();
      var fromLen2 = fromSplit.length;
      var toLen2 = toSplit.length;
      var length2 = fromLen2 < toLen2 ? fromLen2 : toLen2;
      var index2;
      for (index2 = 0; index2 < length2; index2++) {
        if (fromSplit[index2].toLowerCase() !== toSplit[index2].toLowerCase()) break;
      }
      if (index2 === 0) return toOrig;
      if (index2 === length2) {
        if (toLen2 > length2) return toSplit.slice(index2).join("\\");
        if (fromLen2 > length2) {
          var ups = "";
          for (var u = 0; u < fromLen2 - 1 - index2; u++) ups += "..\\";
          return ups + "..";
        }
        return "";
      }
      var prefix = "";
      for (var p = 0; p < fromLen2 - index2; p++) prefix += "..\\";
      return prefix + toSplit.slice(index2).join("\\");
    }

    var fromStart = 0;
    while (fromStart < from.length && from.charCodeAt(fromStart) === CHAR_BACKWARD_SLASH) fromStart++;
    var fromEnd = from.length;
    while (fromEnd - 1 > fromStart && from.charCodeAt(fromEnd - 1) === CHAR_BACKWARD_SLASH) fromEnd--;
    var fromLen = fromEnd - fromStart;

    var toStart = 0;
    while (toStart < to.length && to.charCodeAt(toStart) === CHAR_BACKWARD_SLASH) toStart++;
    var toEnd = to.length;
    while (toEnd - 1 > toStart && to.charCodeAt(toEnd - 1) === CHAR_BACKWARD_SLASH) toEnd--;
    var toLen = toEnd - toStart;

    var length = fromLen < toLen ? fromLen : toLen;
    var lastCommonSep = -1;
    var i = 0;
    for (; i < length; i++) {
      var fromCode = from.charCodeAt(fromStart + i);
      if (fromCode !== to.charCodeAt(toStart + i)) break;
      else if (fromCode === CHAR_BACKWARD_SLASH) lastCommonSep = i;
    }

    if (i !== length) {
      if (lastCommonSep === -1) return toOrig;
    } else {
      if (toLen > length) {
        if (to.charCodeAt(toStart + i) === CHAR_BACKWARD_SLASH) {
          return toOrig.slice(toStart + i + 1);
        }
        if (i === 2) return toOrig.slice(toStart + i);
      }
      if (fromLen > length) {
        if (from.charCodeAt(fromStart + i) === CHAR_BACKWARD_SLASH) lastCommonSep = i;
        else if (i === 2) lastCommonSep = 3;
      }
      if (lastCommonSep === -1) lastCommonSep = 0;
    }

    var out = "";
    for (i = fromStart + lastCommonSep + 1; i <= fromEnd; ++i) {
      if (i === fromEnd || from.charCodeAt(i) === CHAR_BACKWARD_SLASH) {
        out += out.length === 0 ? ".." : "\\..";
      }
    }
    toStart += lastCommonSep;
    if (out.length > 0) return out + toOrig.slice(toStart, toEnd);
    if (toOrig.charCodeAt(toStart) === CHAR_BACKWARD_SLASH) ++toStart;
    return toOrig.slice(toStart, toEnd);
  }

  function win32ToNamespacedPath(path) {
    if (typeof path !== "string" || path.length === 0) return path;
    var resolvedPath = win32Resolve(path);
    if (resolvedPath.length <= 2) return path;
    if (resolvedPath.charCodeAt(0) === CHAR_BACKWARD_SLASH) {
      if (resolvedPath.charCodeAt(1) === CHAR_BACKWARD_SLASH) {
        var code = resolvedPath.charCodeAt(2);
        if (code !== CHAR_QUESTION_MARK && code !== CHAR_DOT) {
          return "\\\\?\\UNC\\" + resolvedPath.slice(2);
        }
      }
    } else if (isWindowsDeviceRoot(resolvedPath.charCodeAt(0)) &&
               resolvedPath.charCodeAt(1) === CHAR_COLON &&
               resolvedPath.charCodeAt(2) === CHAR_BACKWARD_SLASH) {
      return "\\\\?\\" + resolvedPath;
    }
    return resolvedPath;
  }

  function win32Dirname(path) {
    path = String(path);
    var len = path.length;
    if (len === 0) return ".";
    var rootEnd = -1;
    var offset = 0;
    var code = path.charCodeAt(0);
    if (len === 1) return isPathSeparator(code) ? path : ".";

    if (isPathSeparator(code)) {
      rootEnd = offset = 1;
      if (isPathSeparator(path.charCodeAt(1))) {
        var j = 2;
        var last = j;
        while (j < len && !isPathSeparator(path.charCodeAt(j))) j++;
        if (j < len && j !== last) {
          last = j;
          while (j < len && isPathSeparator(path.charCodeAt(j))) j++;
          if (j < len && j !== last) {
            last = j;
            while (j < len && !isPathSeparator(path.charCodeAt(j))) j++;
            if (j === len) return path;
            if (j !== last) rootEnd = offset = j + 1;
          }
        }
      }
    } else if (isWindowsDeviceRoot(code) && path.charCodeAt(1) === CHAR_COLON) {
      rootEnd = len > 2 && isPathSeparator(path.charCodeAt(2)) ? 3 : 2;
      offset = rootEnd;
    }

    var end = -1;
    var matchedSlash = true;
    for (var i = len - 1; i >= offset; --i) {
      if (isPathSeparator(path.charCodeAt(i))) {
        if (!matchedSlash) {
          end = i;
          break;
        }
      } else {
        matchedSlash = false;
      }
    }
    if (end === -1) {
      if (rootEnd === -1) return ".";
      end = rootEnd;
    }
    return path.slice(0, end);
  }

  function win32Basename(path, suffix) {
    path = String(path);
    var start = 0;
    var end = -1;
    var matchedSlash = true;
    if (path.length >= 2 && isWindowsDeviceRoot(path.charCodeAt(0)) &&
        path.charCodeAt(1) === CHAR_COLON) {
      start = 2;
    }
    if (suffix !== undefined && String(suffix).length > 0 && String(suffix).length <= path.length) {
      suffix = String(suffix);
      if (suffix === path) return "";
      var extIdx = suffix.length - 1;
      var firstNonSlashEnd = -1;
      for (var i = path.length - 1; i >= start; --i) {
        var code = path.charCodeAt(i);
        if (isPathSeparator(code)) {
          if (!matchedSlash) {
            start = i + 1;
            break;
          }
        } else {
          if (firstNonSlashEnd === -1) {
            matchedSlash = false;
            firstNonSlashEnd = i + 1;
          }
          if (extIdx >= 0) {
            if (code === suffix.charCodeAt(extIdx)) {
              if (--extIdx === -1) end = i;
            } else {
              extIdx = -1;
              end = firstNonSlashEnd;
            }
          }
        }
      }
      if (start === end) end = firstNonSlashEnd;
      else if (end === -1) end = path.length;
      return path.slice(start, end);
    }
    for (var k = path.length - 1; k >= start; --k) {
      if (isPathSeparator(path.charCodeAt(k))) {
        if (!matchedSlash) {
          start = k + 1;
          break;
        }
      } else if (end === -1) {
        matchedSlash = false;
        end = k + 1;
      }
    }
    if (end === -1) return "";
    return path.slice(start, end);
  }

  function win32Extname(path) {
    path = String(path);
    var start = 0;
    var startDot = -1;
    var startPart = 0;
    var end = -1;
    var matchedSlash = true;
    var preDotState = 0;
    if (path.length >= 2 && path.charCodeAt(1) === CHAR_COLON &&
        isWindowsDeviceRoot(path.charCodeAt(0))) {
      start = startPart = 2;
    }
    for (var i = path.length - 1; i >= start; --i) {
      var code = path.charCodeAt(i);
      if (isPathSeparator(code)) {
        if (!matchedSlash) {
          startPart = i + 1;
          break;
        }
        continue;
      }
      if (end === -1) {
        matchedSlash = false;
        end = i + 1;
      }
      if (code === CHAR_DOT) {
        if (startDot === -1) startDot = i;
        else if (preDotState !== 1) preDotState = 1;
      } else if (startDot !== -1) {
        preDotState = -1;
      }
    }
    if (startDot === -1 || end === -1 || preDotState === 0 ||
        (preDotState === 1 && startDot === end - 1 && startDot === startPart + 1)) {
      return "";
    }
    return path.slice(startDot, end);
  }

  function win32Parse(path) {
    path = String(path);
    var ret = { root: "", dir: "", base: "", ext: "", name: "" };
    if (path.length === 0) return ret;
    var len = path.length;
    var rootEnd = 0;
    var code = path.charCodeAt(0);
    if (len === 1) {
      if (isPathSeparator(code)) {
        ret.root = ret.dir = path;
        return ret;
      }
      ret.base = ret.name = path;
      return ret;
    }
    if (isPathSeparator(code)) {
      rootEnd = 1;
      if (isPathSeparator(path.charCodeAt(1))) {
        var j = 2;
        var last = j;
        while (j < len && !isPathSeparator(path.charCodeAt(j))) j++;
        if (j < len && j !== last) {
          last = j;
          while (j < len && isPathSeparator(path.charCodeAt(j))) j++;
          if (j < len && j !== last) {
            last = j;
            while (j < len && !isPathSeparator(path.charCodeAt(j))) j++;
            if (j === len) rootEnd = j;
            else if (j !== last) rootEnd = j + 1;
          }
        }
      }
    } else if (isWindowsDeviceRoot(code) && path.charCodeAt(1) === CHAR_COLON) {
      if (len <= 2) {
        ret.root = ret.dir = path;
        return ret;
      }
      rootEnd = 2;
      if (isPathSeparator(path.charCodeAt(2))) {
        if (len === 3) {
          ret.root = ret.dir = path;
          return ret;
        }
        rootEnd = 3;
      }
    }
    if (rootEnd > 0) ret.root = path.slice(0, rootEnd);

    var startDot = -1;
    var startPart = rootEnd;
    var end = -1;
    var matchedSlash = true;
    var i = path.length - 1;
    var preDotState = 0;
    for (; i >= rootEnd; --i) {
      code = path.charCodeAt(i);
      if (isPathSeparator(code)) {
        if (!matchedSlash) {
          startPart = i + 1;
          break;
        }
        continue;
      }
      if (end === -1) {
        matchedSlash = false;
        end = i + 1;
      }
      if (code === CHAR_DOT) {
        if (startDot === -1) startDot = i;
        else if (preDotState !== 1) preDotState = 1;
      } else if (startDot !== -1) {
        preDotState = -1;
      }
    }
    if (end !== -1) {
      if (startDot === -1 || preDotState === 0 ||
          (preDotState === 1 && startDot === end - 1 && startDot === startPart + 1)) {
        ret.base = ret.name = path.slice(startPart, end);
      } else {
        ret.name = path.slice(startPart, startDot);
        ret.base = path.slice(startPart, end);
        ret.ext = path.slice(startDot, end);
      }
    }
    if (startPart > 0 && startPart !== rootEnd) ret.dir = path.slice(0, startPart - 1);
    else ret.dir = ret.root;
    return ret;
  }

  function win32Format(object) {
    var dir = object.dir || object.root;
    var base = object.base ||
      ((object.name || "") + formatExt(object.ext));
    if (!dir) return base;
    return dir === object.root ? dir + base : dir + "\\" + base;
  }

  function formatExt(ext) {
    return ext ? (ext[0] === "." ? "" : ".") + ext : "";
  }

  // ------------------------------ exports ------------------------------

  var posix = {
    resolve: posixResolve,
    normalize: posixNormalize,
    isAbsolute: posixIsAbsolute,
    join: posixJoin,
    relative: posixRelative,
    dirname: posixDirname,
    basename: posixBasename,
    extname: posixExtname,
    parse: posixParse,
    format: posixFormat,
    toNamespacedPath: function (path) { return path; },
    sep: "/",
    delimiter: ":",
    toNamespacedPath: function (path) { return path; },
  };

  var win32 = {
    resolve: win32Resolve,
    normalize: win32Normalize,
    isAbsolute: win32IsAbsolute,
    join: win32Join,
    relative: win32Relative,
    dirname: win32Dirname,
    basename: win32Basename,
    extname: win32Extname,
    parse: win32Parse,
    format: win32Format,
    toNamespacedPath: function (path) {
      if (typeof path !== "string" || path.length === 0) return path;
      var resolvedPath = win32Resolve(path);
      if (resolvedPath.length <= 2) return path;
      if (resolvedPath.charCodeAt(0) === CHAR_BACKWARD_SLASH) {
        if (resolvedPath.charCodeAt(1) === CHAR_BACKWARD_SLASH) {
          var code = resolvedPath.charCodeAt(2);
          if (code !== CHAR_QUESTION_MARK && code !== CHAR_DOT) {
            return "\\\\?\\UNC\\" + resolvedPath.slice(2);
          }
        }
      } else if (isWindowsDeviceRoot(resolvedPath.charCodeAt(0)) &&
                 resolvedPath.charCodeAt(1) === CHAR_COLON &&
                 resolvedPath.charCodeAt(2) === CHAR_BACKWARD_SLASH) {
        return "\\\\?\\" + resolvedPath;
      }
      return resolvedPath;
    },
    sep: "\\",
    delimiter: ";",
  };
  posix.posix = posix;
  posix.win32 = win32;
  win32.posix = posix;
  win32.win32 = win32;

  var isWindows = I.host.platform() === "win32";
  var pathModule = isWindows ? win32 : posix;

  shims.path = pathModule;
  shims["node:path"] = pathModule;
  shims["path/posix"] = posix;
  shims["node:path/posix"] = posix;
  shims["path/win32"] = win32;
  shims["node:path/win32"] = win32;
})();

// node:url — WHATWG URL plus legacy helpers (design §6.1).
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var host = I.host;
  var shims = globalThis.__kiana_node_shims;

  function fileURLToPath(input) {
    var value = input instanceof globalThis.URL ? input.href : String(input);
    return host.fileURLToPath(value);
  }

  function pathToFileURL(path) {
    return new globalThis.URL(host.pathToFileURL(String(path)));
  }

  function urlToHttpOptions(urlObject) {
    var protocol = urlObject.protocol || "http:";
    var hostname = urlObject.hostname;
    var pathname = urlObject.pathname || "/";
    var search = urlObject.search || "";
    var options = {
      protocol: protocol,
      hostname: hostname,
      hash: urlObject.hash || "",
      search: search,
      pathname: pathname,
      path: pathname + search,
      href: urlObject.href,
      port: urlObject.port,
      host: urlObject.host,
    };
    if (urlObject.username || urlObject.password) {
      options.auth = decodeURIComponent(urlObject.username || "") +
        (urlObject.password ? ":" + decodeURIComponent(urlObject.password) : "");
    }
    return options;
  }

  function format(urlObject) {
    if (typeof urlObject === "string") return urlObject;
    if (urlObject instanceof globalThis.URL) return urlObject.href;
    if (urlObject.href) return urlObject.href;
    var protocol = urlObject.protocol || "";
    if (protocol && protocol.charAt(protocol.length - 1) !== ":") protocol += ":";
    var slashes = urlObject.slashes || /^(https?|ftp|file):$/.test(protocol);
    var auth = urlObject.auth ? urlObject.auth + "@" : "";
    var hostname = urlObject.hostname || urlObject.host || "";
    var port = urlObject.port ? ":" + urlObject.port : "";
    var pathname = urlObject.pathname || "";
    var search = urlObject.search || (urlObject.query && typeof urlObject.query === "object"
      ? "?" + new globalThis.URLSearchParams(urlObject.query).toString()
      : "");
    var hash = urlObject.hash || "";
    if (hash && hash.charAt(0) !== "#") hash = "#" + hash;
    return protocol + (slashes ? "//" : "") + auth + hostname + port + pathname + search + hash;
  }

  function parse(urlString, parseQueryString) {
    var text = String(urlString);
    try {
      var parsed = new globalThis.URL(text);
      var result = {
        protocol: parsed.protocol,
        slashes: true,
        auth: parsed.username
          ? decodeURIComponent(parsed.username) + (parsed.password ? ":" + decodeURIComponent(parsed.password) : "")
          : null,
        host: parsed.host,
        port: parsed.port || null,
        hostname: parsed.hostname,
        hash: parsed.hash || null,
        search: parsed.search || null,
        query: parseQueryString
          ? Object.fromEntries(parsed.searchParams.entries())
          : (parsed.search ? parsed.search.slice(1) : null),
        pathname: parsed.pathname,
        path: parsed.pathname + parsed.search,
        href: parsed.href,
      };
      return result;
    } catch (_) {
      return {
        protocol: null, slashes: null, auth: null, host: null, port: null,
        hostname: null, hash: null, search: null, query: {}, pathname: text,
        path: text, href: text,
      };
    }
  }

  function resolve(from, to) {
    return new globalThis.URL(to, new globalThis.URL(from)).href;
  }

  function domainToASCII(domain) {
    try {
      return new globalThis.URL("http://" + domain).hostname;
    } catch (_) {
      return "";
    }
  }

  function domainToUnicode(domain) {
    return String(domain);
  }

  var urlObject = {
    URL: globalThis.URL,
    URLSearchParams: globalThis.URLSearchParams,
    fileURLToPath: fileURLToPath,
    pathToFileURL: pathToFileURL,
    urlToHttpOptions: urlToHttpOptions,
    format: format,
    parse: parse,
    resolve: resolve,
    domainToASCII: domainToASCII,
    domainToUnicode: domainToUnicode,
  };

  shims.url = urlObject;
  shims["node:url"] = urlObject;
})();

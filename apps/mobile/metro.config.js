// Learn more: https://docs.expo.dev/guides/customizing-metro/
const { getDefaultConfig } = require("expo/metro-config");

const config = getDefaultConfig(__dirname);

// pdf-lib (서류 작성 도우미) bundles tslib 1.x. With package exports, Metro
// picks its ESM wrapper (modules/index.js), which default-imports a CommonJS
// file flagged __esModule; the interop yields undefined and the app crashes
// at startup. Resolve pdf-lib's tslib to its self-contained ES build instead.
const upstream = config.resolver.resolveRequest;
config.resolver.resolveRequest = (context, moduleName, platform) => {
  const resolve = upstream ?? context.resolveRequest;
  const fromPdfLib = /[\\/]node_modules[\\/]pdf-lib[\\/]/.test(context.originModulePath);
  return resolve(context, fromPdfLib && moduleName === "tslib" ? "tslib/tslib.es6.js" : moduleName, platform);
};

module.exports = config;

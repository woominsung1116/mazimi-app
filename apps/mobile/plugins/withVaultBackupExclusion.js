// Keeps the document vault (Documents/vault on iOS, files/vault on Android) out of
// device backups. The files are encrypted, but their key is device-only keychain
// material (lib/crypto.ts), so a backup copy could never be opened anyway; leaving
// them out also keeps 주민등록번호 작성본 ciphertext off iCloud and Google Drive.
//
// iOS has no app-config switch for this: the app marks the folder with
// NSURLIsExcludedFromBackupKey (URLResourceValues.isExcludedFromBackup) at launch,
// which covers everything stored inside it. Android: this plugin owns the Auto
// Backup rules as an allowlist: once a rule file has any <include>, Auto Backup
// copies only what is included. Only shared preferences are included (never
// expo-secure-store's SecureStore file), so files/vault is never backed up. An
// explicit <exclude domain="file" path="vault/"/> would sit outside every included
// path, which lintVitalRelease rejects (FullBackupContent) and which stops the
// release build. expo-secure-store's own rules are switched off in app.json
// (configureAndroidBackup: false) because this plugin owns the files.
// Native change: takes effect only in a new build, not an OTA update.
const fs = require("fs");
const path = require("path");
const { withAndroidManifest, withAppDelegate, withDangerousMod, AndroidConfig } = require("expo/config-plugins");
const { mergeContents } = require("@expo/config-plugins/build/utils/generateCode");

const TAG = "mazimi-vault-backup-exclusion";
const BACKUP_RULES = "mazimi_backup_rules";
const EXTRACTION_RULES = "mazimi_data_extraction_rules";

const RULES = [
  '    <include domain="sharedpref" path="."/>',
  '    <exclude domain="sharedpref" path="SecureStore"/>',
];
const backupXml = () => [
  '<?xml version="1.0" encoding="utf-8"?>',
  "<!-- Auto Backup, Android 11 and lower. Written by plugins/withVaultBackupExclusion.js. -->",
  "<full-backup-content>",
  ...RULES.map(r => r.slice(2)),
  "</full-backup-content>",
  "",
].join("\n");
const extractionXml = () => [
  '<?xml version="1.0" encoding="utf-8"?>',
  "<!-- Auto Backup and device transfer, Android 12 and higher. Written by plugins/withVaultBackupExclusion.js. -->",
  "<data-extraction-rules>",
  "  <cloud-backup>", ...RULES, "  </cloud-backup>",
  "  <device-transfer>", ...RULES, "  </device-transfer>",
  "</data-extraction-rules>",
  "",
].join("\n");

const SWIFT = [
  "    // Document vault: never in iCloud or computer backups (plugins/withVaultBackupExclusion.js).",
  "    if let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first {",
  "      var vault = documents.appendingPathComponent(\"vault\", isDirectory: true)",
  "      try? FileManager.default.createDirectory(at: vault, withIntermediateDirectories: true)",
  "      var values = URLResourceValues()",
  "      values.isExcludedFromBackup = true",
  "      try? vault.setResourceValues(values)",
  "    }",
].join("\n");

function withIos(config) {
  return withAppDelegate(config, mod => {
    if (mod.modResults.language !== "swift") throw new Error("withVaultBackupExclusion expects a Swift AppDelegate");
    mod.modResults.contents = mergeContents({
      tag: TAG,
      src: mod.modResults.contents,
      newSrc: SWIFT,
      // The first `) -> Bool {` closes application(_:didFinishLaunchingWithOptions:).
      anchor: /\) -> Bool \{/,
      offset: 1,
      comment: "//",
    }).contents;
    return mod;
  });
}

function withAndroid(config) {
  config = withDangerousMod(config, ["android", async mod => {
    const dir = path.join(mod.modRequest.platformProjectRoot, "app/src/main/res/xml");
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, `${BACKUP_RULES}.xml`), backupXml());
    fs.writeFileSync(path.join(dir, `${EXTRACTION_RULES}.xml`), extractionXml());
    return mod;
  }]);
  return withAndroidManifest(config, mod => {
    const app = AndroidConfig.Manifest.getMainApplicationOrThrow(mod.modResults);
    app.$["android:fullBackupContent"] = `@xml/${BACKUP_RULES}`;
    app.$["android:dataExtractionRules"] = `@xml/${EXTRACTION_RULES}`;
    return mod;
  });
}

module.exports = function withVaultBackupExclusion(config) {
  return withAndroid(withIos(config));
};
module.exports.backupXml = backupXml;
module.exports.extractionXml = extractionXml;
module.exports.SWIFT = SWIFT;

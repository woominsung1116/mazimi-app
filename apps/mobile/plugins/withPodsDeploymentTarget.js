// Raises every Pods target below the app's iOS deployment target to it.
// React Native's react_native_post_install already raises each pod's native
// target, but not the pods' resource-bundle targets (the privacy bundles of
// ReachabilitySwift 12.0, RNCAsyncStorage 13.4 and SDWebImage 9.0). Xcode 27
// rejects simulator and device builds with those, so this is not a duplicate
// of RN's hook. The app itself already targets 15.1 (Expo's Podfile default),
// so no supported device changes; the bundles are only aligned with the app.
const fs = require("fs");
const path = require("path");
const { withDangerousMod } = require("expo/config-plugins");
const { mergeContents } = require("@expo/config-plugins/build/utils/generateCode");

const TAG = "mazimi-pods-deployment-target";

function podfileSnippet(target) {
  return [
    `    # Pods below iOS ${target} fail to build with current Xcode; align them with the app.`,
    "    installer.pods_project.targets.each do |pod_target|",
    "      pod_target.build_configurations.each do |build_config|",
    "        current = build_config.build_settings['IPHONEOS_DEPLOYMENT_TARGET']",
    `        if current.nil? || Gem::Version.new(current) < Gem::Version.new('${target}')`,
    `          build_config.build_settings['IPHONEOS_DEPLOYMENT_TARGET'] = '${target}'`,
    "        end",
    "      end",
    "    end",
  ].join("\n");
}

module.exports = function withPodsDeploymentTarget(config, { deploymentTarget = "15.1" } = {}) {
  return withDangerousMod(config, ["ios", async modConfig => {
    const file = path.join(modConfig.modRequest.platformProjectRoot, "Podfile");
    const contents = fs.readFileSync(file, "utf8");
    const merged = mergeContents({
      tag: TAG,
      src: contents,
      newSrc: podfileSnippet(deploymentTarget),
      anchor: /post_install do \|installer\|/,
      offset: 1,
      comment: "#",
    });
    if (!merged.didMerge && !merged.didClear && !contents.includes(TAG)) throw new Error("withPodsDeploymentTarget: Podfile post_install not found");
    fs.writeFileSync(file, merged.contents);
    return modConfig;
  }]);
};
module.exports.podfileSnippet = podfileSnippet;

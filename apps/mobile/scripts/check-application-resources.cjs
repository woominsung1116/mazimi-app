const assert = require('node:assert/strict');
const fs = require('node:fs');
const ts = require('typescript');
require.extensions['.ts'] = (module, filename) => module._compile(ts.transpileModule(fs.readFileSync(filename, 'utf8'), {compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText, filename);
const {destinationForChannel,currentResource,resourceMetaLine} = require('../lib/applicationResources.ts');
const notice = {kind:'information',url:'https://official.example.org/notice'};
const application = {kind:'login_required',url:'https://form.example.org/apply'};
const policy = {notice_destination:notice,destination:application};
assert.equal(destinationForChannel(policy,'online'),application);
assert.equal(destinationForChannel(policy,'visit'),notice);
assert.equal(destinationForChannel(policy,'email'),notice);
assert.equal(destinationForChannel(policy,'online',true),notice);
assert.equal(destinationForChannel({destination:application},'visit'),null);
const previous = {id:'template',url:'https://official.example.org/file',content_hash:'hash',valid_until:'2026-10-08T00:00:00Z'};
const now = Date.parse('2026-10-07T00:00:00Z');
const program = resource => ({application_policy:{resources:{templates:resource?[resource]:[]}}});
assert.equal(currentResource(program(previous),previous,now),previous);
for (const resource of [null,{...previous,content_hash:'changed'},{...previous,url:'https://other.example.org/file'},{...previous,valid_until:'2026-10-06T00:00:00Z'},{...previous,valid_until:'invalid'}]) {
  assert.throws(()=>currentResource(program(resource),previous,now));
}
// A template without a known format has no leading " · "; the day is KST.
assert.equal(resourceMetaLine({file_format:'hwp',verified_at:'2026-10-06T16:00:00Z'}),'HWP · 원본 양식', 'no verification date');
assert.equal(resourceMetaLine({file_format:null,verified_at:'2026-10-06T16:00:00Z'}),'원본 양식');
console.log('Application resource navigation and freshness checks passed (13 cases).');

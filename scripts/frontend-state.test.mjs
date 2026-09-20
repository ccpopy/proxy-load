import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createLatestRequestGuard } from '../web/src/lib/latest-request.ts';
import * as constants from '../web/src/lib/constants.ts';

test('traffic logs default to ten rows and offer only the requested page sizes', () => {
  assert.equal(constants.INITIAL_TRAFFIC_PAGE_SIZE, 10);
  assert.deepEqual(constants.TRAFFIC_PAGE_SIZES, [10, 20, 30, 40, 50]);
});

test('old page/search IPC completion cannot replace the latest request', () => {
  const guard = createLatestRequestGuard();
  const pageOne = guard.begin('1/25/old');
  const pageTwo = guard.begin('2/25/old');
  const search = guard.begin('1/25/new');
  assert.equal(pageOne(), false);
  assert.equal(pageTwo(), false);
  assert.equal(search(), true);
});
test('same parameters still reject an older response', () => {
  const guard = createLatestRequestGuard();
  const old = guard.begin('1/25/');
  const latest = guard.begin('1/25/');
  assert.equal(old(), false);
  assert.equal(latest(), true);
});
import { groupPolicyPayload } from "../web/src/lib/group-policy.ts"
import { updateAction } from "../web/src/lib/update-action.ts"

test("manual update UI never chooses unverified installation", () => {
  assert.equal(updateAction(null),"none")
  assert.equal(updateAction({latest:null,automaticInstallAvailable:false}),"none")
  assert.equal(updateAction({latest:{version:"26.9.20"},automaticInstallAvailable:false}),"manual")
  assert.equal(updateAction({latest:{version:"26.9.20"},automaticInstallAvailable:true}),"install")
})

test("group policy preserves inheritance and bounds the failover hold", () => {
  assert.deepEqual(groupPolicyPayload("inherit", "0"), {algorithm_override:null,sticky_failover_seconds:0})
  assert.deepEqual(groupPolicyPayload("sticky_host", "300"), {algorithm_override:"sticky_host",sticky_failover_seconds:300})
  for (const input of ["", "-1", "1.5", "86401", "NaN"]) assert.throws(() => groupPolicyPayload("sticky_host", input))
  assert.throws(() => groupPolicyPayload("unknown", "0"))
})

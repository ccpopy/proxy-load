import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createLatestRequestGuard } from '../web/src/lib/latest-request.ts';
import * as constants from '../web/src/lib/constants.ts';
import { advancedSettingsPayload, businessLimitError } from '../web/src/lib/advanced-settings.ts';
import { countProxyHealth, proxyMatchesFilter, proxyHealthView, readinessAllowsNewConnections, probeHealthDescription, probeHealthDetails } from '../web/src/lib/proxy-health.ts';

test('proxy metrics and status filters include disabled proxies shown as offline', () => {
  const proxies = [
    { id: 1, enabled: 1, status: 'active' },
    { id: 2, enabled: 1, status: 'active' },
    { id: 3, enabled: 1, status: 'active' },
    { id: 4, enabled: 0, status: 'inactive' },
    { id: 5, enabled: 0, status: 'inactive' },
    { id: 6, enabled: 1, status: 'degraded' },
    { id: 7, enabled: 1, status: 'unknown' },
    { id: 8, enabled: 1, status: 'testing' },
  ];
  const counts = countProxyHealth(proxies);
  assert.deepEqual(counts, { active: 3, inactive: 2, degraded: 1, unknown: 1, testing: 1 });
  for (const [status, count] of Object.entries(counts)) {
    assert.equal(proxies.filter(proxy => proxyMatchesFilter(proxy, status)).length, count);
  }
  assert.deepEqual(proxies.filter(proxy => proxyMatchesFilter(proxy, 'inactive')).map(proxy => proxy.id), [4, 5]);
  assert.equal(proxies.filter(proxy => proxyMatchesFilter(proxy, 'all')).length, 8);
  assert.equal(proxies.filter(proxy => proxyMatchesFilter(proxy, 'enabled')).length, 6);
  assert.equal(proxies.filter(proxy => proxyMatchesFilter(proxy, 'disabled')).length, 2);
  for (const proxy of proxies.filter(proxy => proxy.enabled === 0)) {
    assert.equal(readinessAllowsNewConnections(proxy), false);
  }
});

test('proxy health statistics are independent of the enable switch and handle an empty list', () => {
  const statuses = ['active', 'inactive', 'degraded', 'unknown', 'testing'];
  const enabled = statuses.map(status => ({ enabled: 1, status }));
  const disabled = enabled.map(proxy => ({ ...proxy, enabled: 0 }));
  const expected = { active: 1, inactive: 1, degraded: 1, unknown: 1, testing: 1 };
  assert.deepEqual(countProxyHealth(enabled), expected);
  assert.deepEqual(countProxyHealth(disabled), expected);
  assert.deepEqual(countProxyHealth([]), { active: 0, inactive: 0, degraded: 0, unknown: 0, testing: 0 });
  for (const proxy of disabled) {
    assert.equal(proxyMatchesFilter(proxy, proxy.status), true);
    assert.equal(proxyMatchesFilter(proxy, 'enabled'), false);
    assert.equal(proxyMatchesFilter(proxy, 'disabled'), true);
  }
});

test('proxy statistics follow health badges without treating all probe failures as offline', () => {
  const failed = { enabled: 1, status: 'active',
    probe_health: { fresh: true, transport_status: 'reachable', last_probe_result: { outcome: 'failure' } } };
  const unreachable = { ...failed, probe_health: { ...failed.probe_health, transport_status: 'unreachable' } };
  const unready = { ...failed, health_policy: { mode: 'required_probe' },
    probe_health: { ...failed.probe_health, readiness_status: 'not_ready' } };
  const stale = { ...unready, probe_health: { ...unready.probe_health, fresh: false } };
  const proxies = [failed, unreachable, unready, stale];
  assert.deepEqual(countProxyHealth(proxies), { active: 0, inactive: 2, degraded: 1, unknown: 1, testing: 0 });
  for (const proxy of proxies) {
    assert.equal(proxyMatchesFilter(proxy, proxyHealthView(proxy).key), true);
  }
  assert.equal(proxyMatchesFilter(failed, 'inactive'), false);
  assert.equal(proxyMatchesFilter(stale, 'inactive'), false);
});

test('readiness badges, filters and admission distinguish failed probe from reachable entry', () => {
  const proxy = { enabled: 1, status: 'active', success_count: 99, fail_count: 7,
    health_policy: { mode: 'required_probe', failure_threshold: 2, recovery_threshold: 2, max_age_seconds: 600 },
    probe_health: { fresh: true, transport_status: 'reachable', readiness_status: 'not_ready',
      consecutive_failures: 2, consecutive_successes: 0, probe_success_count: 0, probe_failure_count: 2,
      last_probe_result: { outcome: 'failure', observed_at: 1000, probe_url: 'http://vpn.test/health', url_source: 'node',
        diagnostics: { phase: 'tunnel_connect', scope: 'target_route', code: { kind: 'timeout' } } } } };
  assert.deepEqual(proxyHealthView(proxy), { key: 'inactive', label: '业务不可用', entry: '代理入口可达' });
  assert.equal(readinessAllowsNewConnections(proxy), false);
  assert.match(probeHealthDescription(proxy, 2000), /tunnel_connect/);
  assert.match(probeHealthDescription(proxy, 2000), /旧版混合历史：成功 99/);
  const degraded = { ...proxy, probe_health: { ...proxy.probe_health, readiness_status: 'degraded', consecutive_failures: 1 } };
  assert.equal(proxyHealthView(degraded).key, 'degraded');
  assert.equal(readinessAllowsNewConnections(degraded), true);
  assert.equal(proxyHealthView({ ...proxy, probe_health: { ...proxy.probe_health, fresh: false } }).label, '业务待验证');
  const general = { ...proxy, health_policy: { ...proxy.health_policy, mode: 'transport_only' } };
  assert.equal(proxyHealthView(general).label, '最近测活失败');
  assert.equal(readinessAllowsNewConnections(general), true);
  assert.equal(readinessAllowsNewConnections({ ...general, enabled: 0 }), false);
});

test('probe details stay independent for wrapping while preserving the tooltip content', () => {
  const general = { enabled: 1, status: 'active', success_count: 91, fail_count: 4 };
  assert.deepEqual(probeHealthDetails(general, 2000), [
    '通用代理：目标失败不作节点级隔离',
    '最近完整成功：尚无',
    '新测活统计起始：尚未开始；未计入失败的探测：0',
    '旧版混合历史：成功 91 / 失败 4（不并入新统计）',
  ]);
  const url = 'http://vpn-only.test/' + 'long-health-resource/'.repeat(12);
  const dedicated = { ...general,
    health_policy: { mode: 'required_probe', failure_threshold: 2, recovery_threshold: 2, max_age_seconds: 600 },
    probe_health: { fresh: false, readiness_status: 'unknown', consecutive_failures: 1, consecutive_successes: 0,
      last_probe_result: { observed_at: 1000, probe_url: url, url_source: 'global', diagnostics: { phase: 'tunnel_connect', scope: 'target_route' } } } };
  for (const proxy of [general, dedicated]) {
    const items = probeHealthDetails(proxy, 2000);
    assert.ok(items.every(item => typeof item === 'string' && !item.includes('\n')));
    assert.equal(items.join('\n'), probeHealthDescription(proxy, 2000));
  }
  assert.ok(probeHealthDetails(dedicated, 2000).includes(`探测地址（继承全局）：${url}`));
  assert.ok(probeHealthDetails(dedicated, 2000).includes('已阻止该节点全部新业务连接；后台继续探测'));
});

test('concurrency settings retain defaults, validate bounds and never save runtime diagnostics', () => {
  const config = { ...constants.defaultAdvanced, effective_concurrency: { max_connections: 2 } };
  assert.equal(config.target_quality_mode, 'off');
  assert.equal(config.max_connections, 1024);
  assert.equal(config.max_handshakes, 128);
  assert.equal(config.max_global_dials, 64);
  assert.equal(config.max_proxy_dials, 32);
  assert.equal(businessLimitError(config), null);
  assert.ok(businessLimitError({ ...config, max_connections: 0 }));
  assert.ok(businessLimitError({ ...config, max_proxy_dials: 65 }));
  assert.ok(businessLimitError({ ...config, max_global_dials: 64.5 }));
  assert.equal('effective_concurrency' in advancedSettingsPayload(config), false);
});

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

const API_BASE = document.querySelector('meta[name="api-base"]')?.content || "";
const statusTextEl = document.querySelector("#status-indicator");

// 时区与格式化器
const DISPLAY_TZ = "Asia/Shanghai";
const dtfDateTime = new Intl.DateTimeFormat("zh-CN", {
  timeZone: DISPLAY_TZ,
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  hour12: false,
});
const dtfHourMinute = new Intl.DateTimeFormat("zh-CN", {
  timeZone: DISPLAY_TZ,
  hour: "2-digit",
  minute: "2-digit",
  hour12: false,
});

const TEST_BUTTON_HTML = {
  idle: '<i class="fas fa-bolt mr-1"></i>连通性测试',
  loading: '<i class="fas fa-spinner fa-spin mr-1"></i>连通性测试中...'
};
const BATCH_TEST_BUTTON_HTML = {
  idle: '<i class="fas fa-bolt mr-2"></i>一键连通性测试',
  loading: '<i class="fas fa-spinner fa-spin mr-2"></i>批量测试中...'
};

window.currentProxyFilter = "all";

// 存储正在测试的按钮状态
const testingButtons = new Map();
const batchTestState = { running: false, token: null };
const isProxyEnabled = (proxy) => proxy && (proxy.enabled === 1 || proxy.enabled === true);

// WebSocket 管理
const wsManager = {
  ws: null,
  reconnectTimer: null,
  messageQueue: [],
  maxQueueSize: 5,
  isPageActive: true,
  // 标记是否正在处理积压消息
  isProcessingBacklog: false,
  // 临时缓冲区
  backlogBuffer: [],
  // 延迟处理定时器
  backlogTimer: null,
  queueSummary: {
    statusChanges: { online: 0, offline: 0 },
    testCompleted: null,
    testProgress: { current: 0, total: 0 },
    lastUpdate: Date.now()
  },

  connect () {
    if (this.ws) this.ws.close();

    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    const wsUrl = `${protocol}//${window.location.host}`;
    this.ws = new WebSocket(wsUrl);

    this.ws.onopen = () => this.handleOpen();
    this.ws.onmessage = (event) => this.handleMessage(event);
    this.ws.onclose = () => this.handleClose();
    this.ws.onerror = (error) => this.handleError(error);
  },

  handleOpen () {
    statusManager.setStatus("online");
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }

    // 如果是重连后的首次连接，启动积压处理模式
    if (!this.isPageActive || this.isProcessingBacklog) {
      this.startBacklogProcessing();
    }
  },

  handleMessage (event) {
    const message = JSON.parse(event.data);

    // 如果页面不活跃，聚合消息
    if (!this.isPageActive) {
      this.queueMessage(message);
      return;
    }

    // 如果正在处理积压，缓冲消息
    if (this.isProcessingBacklog) {
      this.bufferBacklogMessage(message);
      return;
    }

    // 正常处理消息
    messageHandler.process(message);
  },

  handleClose () {
    statusManager.setStatus("reconnecting");
    this.reconnectTimer = setTimeout(() => this.connect(), 5000);
  },

  handleError (error) {
    console.error("WebSocket错误:", error);
    statusManager.setStatus("error");
  },

  startBacklogProcessing () {
    this.isProcessingBacklog = true;
    this.backlogBuffer = [];

    // 设置1秒的收集期，收集所有积压消息
    if (this.backlogTimer) clearTimeout(this.backlogTimer);

    this.backlogTimer = setTimeout(() => {
      this.processBacklog();
    }, 1000);
  },

  bufferBacklogMessage (message) {
    // 聚合相同类型的消息
    if (message.type === 'proxies_status_changed' && message.data?.changes) {
      message.data.changes.forEach(change => {
        if (change.newStatus === 'active') this.queueSummary.statusChanges.online++;
        if (change.newStatus === 'inactive') this.queueSummary.statusChanges.offline++;
      });
    } else if (message.type === 'periodic_test_completed') {
      this.queueSummary.testCompleted = message.data;
    } else if (message.type === 'batch_test_completed' && message.data) {
      // 记录最新的测试进度
      if (message.data.batch && message.data.totalBatches) {
        this.queueSummary.testProgress = {
          current: message.data.batch,
          total: message.data.totalBatches
        };
      }
    }

    // 只保存需要后续处理的消息（如更新界面）
    if (['proxy_updated', 'proxy_tested', 'proxy_created', 'proxy_deleted'].includes(message.type)) {
      this.backlogBuffer.push(message);
    }
  },

  processBacklog () {
    this.isProcessingBacklog = false;

    const summary = this.queueSummary;
    let notification = null;

    // 优先显示最新的测试结果，因为它代表了当前最终状态
    if (summary.testCompleted) {
      const data = summary.testCompleted;
      notification = {
        message: `当前状态: ${data?.testResults?.success ?? 0} / ${data?.testResults?.total ?? 0} 代理可用`,
        type: (data?.testResults?.failed ?? 0) > 0 ? "warning" : "success"
      };
    } else if (summary.statusChanges.online > 0 || summary.statusChanges.offline > 0) {
      // 如果没有测试结果，但有状态变化，则显示变化的汇总
      const statusParts = [];
      if (summary.statusChanges.online > 0) statusParts.push(`${summary.statusChanges.online}个上线`);
      if (summary.statusChanges.offline > 0) statusParts.push(`${summary.statusChanges.offline}个离线`);
      notification = {
        message: `代理状态变化: ${statusParts.join(", ")}`,
        type: summary.statusChanges.offline > 0 ? "warning" : "success"
      };
    }

    if (notification) {
      uiUtils.showToast(notification.message, notification.type);
    }


    // 静默处理需要更新界面的消息
    this.backlogBuffer.forEach(msg => {
      messageHandler.processSilent(msg);
    });

    // 重置汇总数据
    this.resetQueueSummary();
    this.backlogBuffer = [];
  },

  queueMessage (message) {
    const now = Date.now();

    // 重置过期的队列摘要
    if (now - this.queueSummary.lastUpdate > 30000) {
      this.resetQueueSummary();
    }

    // 聚合消息（与bufferBacklogMessage类似）
    if (message.type === 'proxies_status_changed' && message.data?.changes) {
      message.data.changes.forEach(change => {
        if (change.newStatus === 'active') this.queueSummary.statusChanges.online++;
        if (change.newStatus === 'inactive') this.queueSummary.statusChanges.offline++;
      });
    } else if (message.type === 'periodic_test_completed') {
      this.queueSummary.testCompleted = message.data;
    } else if (message.type === 'batch_test_completed' && message.data) {
      if (message.data.batch && message.data.totalBatches) {
        this.queueSummary.testProgress = {
          current: message.data.batch,
          total: message.data.totalBatches
        };
      }
    }

    this.queueSummary.lastUpdate = now;
  },

  processQueue () {
    if (!this.isPageActive) return;

    // 启动积压处理模式，收集1秒内的所有消息
    this.startBacklogProcessing();
  },

  setPageActive (active) {
    const wasInactive = !this.isPageActive;
    this.isPageActive = active;

    if (active && wasInactive) {
      // 页面从非活跃变为活跃，启动积压处理
      this.startBacklogProcessing();

      if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
        this.connect();
      }
    }
  },

  resetQueueSummary () {
    this.queueSummary = {
      statusChanges: { online: 0, offline: 0 },
      testCompleted: null,
      testProgress: { current: 0, total: 0 },
      lastUpdate: Date.now()
    };
  }
};

// 状态管理器
const statusManager = {
  statuses: {
    online: { icon: "fa-circle text-green-500", text: "系统运行中" },
    reconnecting: { icon: "fa-circle text-yellow-500", text: "重新连接中..." },
    error: { icon: "fa-circle text-red-500", text: "连接异常" },
    offline: { icon: "fa-circle text-red-500", text: "系统离线中" }
  },

  setStatus (type) {
    const status = this.statuses[type] || this.statuses.error;
    statusTextEl.innerHTML = `<i class="fas ${status.icon} text-xs mr-1"></i> ${status.text}`;
  }
};

// 消息处理器
const messageHandler = {
  handlers: {
    proxy_updated: () => proxyManager.loadList(),
    proxy_tested: () => proxyManager.loadList(),
    proxy_created: () => proxyManager.loadList(),
    proxy_deleted: () => proxyManager.loadList(),

    request_logged: (data) => {
      if (uiUtils.isSystemStatusVisible()) {
        chartManager.updateStatisticsPartial(data);
        trafficLogManager.pushRealtime(data);
      }
    },

    batch_test_completed: (data) => {
      // 只在非积压处理模式下显示进度
      if (!wsManager.isProcessingBacklog && data?.batch && data?.totalBatches) {
        uiUtils.showToast(`测试进度: ${data.batch}/${data.totalBatches}`, "info");
      }
    },

    periodic_test_completed: (data) => {
      if (data.overview) chartManager.updateOverviewKPIs(data.overview);
      if (window.__systemStatusInited) {
        if (data.hourly) chartManager.updateHourlyChart(data.hourly);
        if (data.proxyUsage) chartManager.updateProxyUsageChart(data.proxyUsage);
      }

      // 只在非积压处理模式下显示通知
      if (!wsManager.isProcessingBacklog) {
        uiUtils.showToast(
          `测试完成: ${data?.testResults?.success ?? 0}/${data?.testResults?.total ?? 0} 代理可用`,
          (data?.testResults?.failed ?? 0) > 0 ? "warning" : "success"
        );
      }
    },

    proxies_status_changed: (data) => {
      proxyManager.loadList();

      // 只在非积压处理模式下显示通知
      if (!wsManager.isProcessingBacklog && data?.changes?.length > 0) {
        const changes = data.changes;
        const online = changes.filter(c => c.newStatus === "active").length;
        const offline = changes.filter(c => c.newStatus === "inactive").length;
        if (online > 0 || offline > 0) {
          const parts = [];
          if (online > 0) parts.push(`${online}个代理上线`);
          if (offline > 0) parts.push(`${offline}个代理离线`);
          uiUtils.showToast(`状态变化: ${parts.join(", ")}`, offline > online ? "warning" : "success");
        }
      }
    },

    periodic_test_error: (data) => {
      // 错误始终显示
      uiUtils.showToast("定期测试失败: " + (data?.error || ""), "error");
    },

    stats_update: (data) => {
      if (window.__systemStatusInited) {
        chartManager.updateChartsPartial(data);
      }
    }
  },

  process (message) {
    const handler = this.handlers[message.type];
    if (handler) handler(message.data);
  },

  processSilent (message) {
    const silentHandlers = {
      proxy_updated: () => proxyManager.loadList(),
      proxy_tested: () => proxyManager.loadList(),
      proxy_created: () => proxyManager.loadList(),
      proxy_deleted: () => proxyManager.loadList(),
      request_logged: (data) => {
        if (uiUtils.isSystemStatusVisible()) {
          chartManager.updateStatisticsPartial(data);
          trafficLogManager.pushRealtime(data);
        }
      },
      periodic_test_completed: (data) => {
        if (data.overview) chartManager.updateOverviewKPIs(data.overview);
        if (window.__systemStatusInited) {
          if (data.hourly) chartManager.updateHourlyChart(data.hourly);
          if (data.proxyUsage) chartManager.updateProxyUsageChart(data.proxyUsage);
        }
      }
    };

    const handler = silentHandlers[message.type];
    if (handler) handler(message.data);
  }
};

// 日期工具
const dateUtils = {
  parse (input) {
    if (input == null) return null;
    if (typeof input === "number") return new Date(input);

    let s = String(input).trim();
    if (/^\d+$/.test(s)) return new Date(Number(s));
    if (/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}(:\d{2})?$/.test(s)) {
      s = s.replace(" ", "T") + "Z";
    }
    if (/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2})?$/.test(s)) {
      s = s + "Z";
    }

    const d = new Date(s);
    if (isNaN(d.getTime())) {
      console.warn('Invalid date format:', input);
      return null;
    }
    return d;
  },

  format (dateInput) {
    const d = this.parse(dateInput);
    return d ? dtfDateTime.format(d) : String(dateInput ?? "");
  },

  formatHourLabel (dateInput) {
    const d = this.parse(dateInput);
    return d ? dtfHourMinute.format(d) : "";
  }
};

// 数据提取工具
const dataExtractor = {
  getRawHour (item) {
    return item?.hour ?? item?.time ?? item?.timestamp ?? item?.ts ?? item?.x ?? null;
  },

  getSuccess (item) {
    return item?.success_requests ?? item?.succ ?? item?.success ?? 0;
  },

  getFailed (item) {
    return item?.failed_requests ?? item?.fail ?? item?.failed ?? 0;
  },

  getAvgRt (item) {
    return Math.round(item?.avg_response_time ?? item?.avgRt ?? item?.avg_response ?? 0) || 0;
  }
};

// UI 工具
const uiUtils = {
  showToast (message, type = "info", opts = {}) {
    const defaults = { success: 2400, info: 3000, warning: 4200, error: 5200 };
    const duration = Number(opts.duration ?? defaults[type] ?? 3000);
    const sticky = !!opts.sticky;

    let layer = document.querySelector("#toast-layer");
    if (!layer) {
      layer = document.createElement("div");
      layer.id = "toast-layer";
      document.body.appendChild(layer);
    }

    const icons = {
      success: "fa-check-circle",
      error: "fa-times-circle",
      warning: "fa-exclamation-triangle",
      info: "fa-info-circle"
    };

    const el = document.createElement("div");
    el.className = `toast toast-${type}`;
    el.innerHTML = `
      <span class="toast-accent"></span>
      <i class="fas ${icons[type] || icons.info} toast-icon"></i>
      <span class="toast-content">${message}</span>
      <button class="toast-close" aria-label="关闭">&times;</button>
    `;
    layer.appendChild(el);

    requestAnimationFrame(() => el.classList.add("in"));

    let timer = null;
    const remove = () => {
      el.classList.remove("in");
      el.classList.add("out");
      setTimeout(() => el.remove(), 250);
    };

    if (!sticky) {
      timer = setTimeout(remove, duration);
      el.addEventListener("mouseenter", () => timer && clearTimeout(timer));
      el.addEventListener("mouseleave", () => {
        timer = setTimeout(remove, Math.max(1200, duration / 2));
      });
    }

    el.querySelector(".toast-close").addEventListener("click", remove);
  },

  animateNumber (el, from, to, duration = 600) {
    const start = performance.now();
    const step = (t) => {
      const p = Math.min(1, (t - start) / duration);
      const value = Math.round(from + (to - from) * p);
      el.textContent = value.toLocaleString();
      if (p < 1) requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  },

  isSystemStatusVisible () {
    return !document.querySelector("#system-status").classList.contains("hidden");
  },

  mbpsToMbs (mbps) {
    const M = mbps / 1048576;
    return (M / 8).toFixed(2);
  }
};

// 代理管理器
const proxyManager = {
  allProxies: [],
  filteredProxies: [],
  renderSkeleton () {
    const container = document.querySelector("#proxy-cards");
    container.innerHTML = `
      <div class="skeleton-loader">
        ${Array(3).fill('').map(() => `
          <div class="card animate-pulse">
            <div class="h-4 bg-gray-200 rounded w-3/4 mb-4"></div>
            <div class="h-3 bg-gray-200 rounded w-1/2 mb-2"></div>
            <div class="h-3 bg-gray-200 rounded w-2/3"></div>
          </div>
        `).join('')}
      </div>
    `;
  },

  async loadList () {
    try {
      const response = await fetch(`${API_BASE}/api/proxies`);
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const proxies = await response.json();
      this.allProxies = proxies;
      const filtered = this.applyFilter(proxies);
      this.filteredProxies = filtered;
      updateProxyFilterCounts(proxies);
      statusManager.setStatus("online");
      this.renderCards(filtered);
    } catch (error) {
      statusManager.setStatus("offline");
      console.error("加载代理列表失败:", error);
    }
  },

  renderCards (proxies) {
    const container = document.querySelector("#proxy-cards");

    if (proxies.length === 0) {
      const hasAnyProxies = Array.isArray(this.allProxies) && this.allProxies.length > 0;
      const emptyTitle = hasAnyProxies ? "暂无符合筛选条件的代理" : "暂无代理配置";
      const emptyHint = hasAnyProxies ? "请调整筛选条件查看其他代理" : "点击\"新增代理配置\"按钮添加";
      container.innerHTML = `
        <div class="card text-center py-12 text-gray-500">
          <i class="fas fa-inbox text-4xl mb-3"></i>
          <p>${emptyTitle}</p>
          <p class="text-sm mt-2">${emptyHint}</p>
        </div>
      `;
      return;
    }

    container.innerHTML = proxies.map((proxy, index) => this.createCard(proxy, index)).join("");
  },

  applyFilter (proxies) {
    const mode = window.currentProxyFilter || "all";
    if (mode === "enabled") {
      return proxies.filter(isProxyEnabled);
    }
    if (mode === "disabled") {
      return proxies.filter(proxy => !isProxyEnabled(proxy));
    }
    if (mode === "active") {
      return proxies.filter(proxy => isProxyEnabled(proxy) && proxy.status === "active");
    }
    if (mode === "inactive") {
      return proxies.filter(proxy => isProxyEnabled(proxy) && proxy.status === "inactive");
    }
    if (mode === "testing") {
      return proxies.filter(proxy => isProxyEnabled(proxy) && proxy.status === "testing");
    }
    return proxies;
  },

  createCard (proxy, index) {
    const priorityBadge = window.currentLoadMode === "manual"
      ? `<div class="absolute w-8 h-8 bg-blue-500 text-white rounded-full flex items-center justify-center font-bold text-sm order-num">${index + 1}</div>`
      : "";

    const priorityInfo = window.currentLoadMode === "manual"
      ? `<span class="ml-2 px-2 py-1 text-xs bg-blue-100 text-blue-800 rounded">优先级: ${proxy.priority || 999}</span>`
      : "";

    const status = this.getProxyStatus(proxy);
    const stats = this.getProxyStats(proxy);
    const cardStateClass = !proxy.enabled || proxy.enabled === 0
      ? "is-disabled"
      : (proxy.status === "inactive" ? "is-offline" : "");

    return `
      <div class="card proxy-card hover:shadow-md transition-shadow relative ${cardStateClass} ${window.currentLoadMode === "manual" ? "pl-16" : ""}">
        ${priorityBadge}
        <div class="flex justify-between items-center">
          <div class="flex-1">
            <div class="flex items-center mb-2">
              <h2 class="font-semibold text-lg">${proxy.name}</h2>
              <span class="ml-3 px-2 py-1 text-xs rounded-full ${status.class}">${status.text}</span>
              ${priorityInfo}
            </div>
            <div class="space-y-1 text-sm text-gray-600 sort-list">
              ${stats}
            </div>
          </div>
          <div class="flex flex-col space-y-2">
            ${this.createActionButtons(proxy)}
          </div>
        </div>
      </div>
    `;
  },

  getProxyStatus (proxy) {
    if (!proxy.enabled || proxy.enabled === 0) {
      return { text: "未启用", class: "bg-gray-100 text-gray-600" };
    }

    const statusMap = {
      active: { text: "在线", class: "bg-green-100 text-green-800" },
      testing: { text: "测试中", class: "bg-yellow-100 text-yellow-800" },
      inactive: { text: "离线", class: "bg-red-100 text-red-800" }
    };

    return statusMap[proxy.status] || { text: "未知", class: "bg-gray-100 text-gray-600" };
  },

  getProxyStats (proxy) {
    const stats = [
      `<p><i class="fas fa-server mr-2 w-4"></i>类型: ${proxy.type.toUpperCase()}</p>`,
      `<p><i class="fas fa-network-wired mr-2 w-4"></i>地址: ${proxy.host}:${proxy.port}</p>`
    ];

    if (proxy.username) {
      stats.push(`<p><i class="fas fa-user mr-2 w-4"></i>用户: ${proxy.username}</p>`);
    }
    if (proxy.last_test) {
      stats.push(`<p><i class="fas fa-clock mr-2 w-4"></i>最后测试: ${dateUtils.format(proxy.last_test)}</p>`);
    }
    if (proxy.response_time) {
      stats.push(`<p><i class="fa-solid fa-timer mr-2 w-4"></i>响应时间: ${proxy.response_time}ms</p>`);
    }

    stats.push(`<p><i class="fas fa-chart-bar mr-2 w-4"></i>成功/失败: <span class="text-green-600">${proxy.success_count || 0}</span> / <span class="text-red-600">${proxy.fail_count || 0}</span></p>`);

    if (window.currentLoadMode === "auto" && typeof proxy._score === "number") {
      stats.push(`<p><i class="fas fa-star mr-2 w-4"></i>智能评分: <span class="font-semibold">${proxy._score}</span><span class="text-gray-400 text-xs ml-1">（越低越好）</span></p>`);

      if (proxy._recentTotal > 0) {
        const avgRt = proxy._avgSuccRt ? `<span class="text-blue-600 ml-4">平均响应时间: ${proxy._avgSuccRt}ms</span>` : "";
        stats.push(`<p><i class="fas fa-chart-line mr-2 w-4"></i>近15分钟: <span class="text-green-600">${proxy._recentTotal - proxy._recentFails}</span> / <span class="text-red-600">${proxy._recentFails}</span>${avgRt}</p>`);
      }
    }

    if (proxy.bandwidth_bps) {
      stats.push(`<p><i class="fas fa-tachometer-alt-fast mr-2 w-4"></i>带宽速度：<span>${uiUtils.mbpsToMbs(proxy.bandwidth_bps) || 0} MB/s</span></p>`);
    }

    return stats.join("");
  },

  createActionButtons (proxy) {
    const proxyKey = String(proxy.id);
    const isTesting = testingButtons.has(proxyKey);
    const testButtonHtml = isTesting ? TEST_BUTTON_HTML.loading : TEST_BUTTON_HTML.idle;
    const testButtonDisabled = !proxy.enabled || proxy.enabled === 0 || isTesting;
    const buttons = [
      `<button data-test-proxy-id="${proxy.id}" data-proxy-enabled="${proxy.enabled ? 1 : 0}" onclick="testProxy(event, ${proxy.id}, { bandwidth:false })" class="btn-success" ${testButtonDisabled ? 'disabled' : ''}>
        ${testButtonHtml}
      </button>`,
      `<div class="flex justify-end">
        <button onclick="editProxy(${proxy.id})" class="btn-primary"><i class="fas fa-edit mr-1"></i>编辑</button>
      </div>`
    ];

    if (window.currentLoadMode === "manual") {
      buttons.push(`<div class="flex justify-end">
        <button onclick="adjustPriority(${proxy.id}, ${proxy.priority || 999})" class="btn-secondary">
          <i class="fas fa-sort mr-1"></i>调序
        </button>
      </div>`);
    }

    buttons.push(`<div class="flex justify-end">
      <button onclick="deleteProxy(${proxy.id})" class="btn-danger"><i class="fas fa-trash mr-1"></i>删除</button>
    </div>`);

    return buttons.join("");
  }
};

function createTestToken (prefix = "test") {
  return `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function updateTestButtonState (proxyId) {
  const btn = document.querySelector(`[data-test-proxy-id="${proxyId}"]`);
  if (!btn) return;
  const isTesting = testingButtons.has(String(proxyId));
  const isEnabled = btn.dataset.proxyEnabled === "1";
  btn.innerHTML = isTesting ? TEST_BUTTON_HTML.loading : TEST_BUTTON_HTML.idle;
  btn.disabled = !isEnabled || isTesting;
}

function setProxyTestingState (proxyId, token) {
  testingButtons.set(String(proxyId), { token });
  updateTestButtonState(proxyId);
}

function clearProxyTestingState (proxyId, token) {
  const key = String(proxyId);
  const state = testingButtons.get(key);
  if (state && state.token === token) {
    testingButtons.delete(key);
    updateTestButtonState(proxyId);
    return true;
  }
  return false;
}

function setBatchTestButtonState (running) {
  const btn = document.querySelector("#batch-test-btn");
  if (!btn) return;
  btn.innerHTML = running ? BATCH_TEST_BUTTON_HTML.loading : BATCH_TEST_BUTTON_HTML.idle;
  btn.disabled = running;
}

function updateProxyFilterCounts (proxies) {
  const tabContainer = document.querySelector("#proxy-filter-tabs");
  if (!tabContainer) return;
  const list = Array.isArray(proxies) ? proxies : [];
  const enabledList = list.filter(isProxyEnabled);
  const counts = {
    all: list.length,
    enabled: enabledList.length,
    disabled: list.length - enabledList.length,
    active: enabledList.filter(proxy => proxy.status === "active").length,
    inactive: enabledList.filter(proxy => proxy.status === "inactive").length,
    testing: enabledList.filter(proxy => proxy.status === "testing").length
  };

  Object.entries(counts).forEach(([key, value]) => {
    const el = tabContainer.querySelector(`[data-count-for="${key}"]`);
    if (el) el.textContent = value;
  });
}

function applyProxyFilter (filterValue) {
  const nextFilter = filterValue || window.currentProxyFilter || "all";
  window.currentProxyFilter = nextFilter;
  const tabs = document.querySelectorAll("#proxy-filter-tabs .filter-tab");
  if (tabs.length > 0) {
    tabs.forEach(btn => {
      btn.classList.toggle("active", btn.dataset.filter === nextFilter);
    });
  }
  const filtered = proxyManager.applyFilter(proxyManager.allProxies || []);
  proxyManager.filteredProxies = filtered;
  proxyManager.renderCards(filtered);
}

// 图表管理器
const chartManager = {
  charts: {},
  lastOverview: null,
  chartDataState: {
    traffic: { xAxis: [], success: [], failed: [] },
    latency: { xAxis: [], data: [] },
    proxyUsage: { yAxis: [], data: [] },
    targets: { yAxis: [], data: [] }
  },

  init () {
    // 销毁已存在的图表实例
    Object.keys(this.charts).forEach(key => {
      if (this.charts[key]) {
        this.charts[key].dispose();
        this.charts[key] = null;
      }
    });

    this.charts.traffic = echarts.init(document.querySelector("#chart-traffic"));
    this.charts.latency = echarts.init(document.querySelector("#chart-latency"));
    this.charts.proxyUsage = echarts.init(document.querySelector("#chart-proxy-usage"));
    this.charts.targets = echarts.init(document.querySelector("#chart-targets"));

    this.setInitialOptions();
    this.refresh();

    window.removeEventListener("resize", this.resize.bind(this));
    window.addEventListener("resize", this.resize.bind(this));
  },

  setInitialOptions () {
    this.charts.traffic.setOption({
      title: { text: "最近24小时请求趋势", left: "center", textStyle: { fontSize: 12 } },
      tooltip: {
        trigger: "axis",
        axisPointer: {
          type: "cross",
          label: {
            backgroundColor: '#6a7985'
          }
        }
      },
      legend: { bottom: 0, data: ["成功", "失败"] },
      grid: { left: 40, right: 20, top: 40, bottom: 40 },
      xAxis: { type: "category", data: [] },
      yAxis: { type: "value" },
      series: [
        { name: "成功", type: "line", areaStyle: {}, smooth: true, data: [], animation: true, itemStyle: { color: '#10b981' }, areaStyle: { color: 'rgba(16, 185, 129, 0.2)' } },
        { name: "失败", type: "line", areaStyle: {}, smooth: true, data: [], animation: true, itemStyle: { color: '#ef4444' }, areaStyle: { color: 'rgba(239, 68, 68, 0.2)' } }
      ],
      animation: true,
      animationDuration: 300
    });

    this.charts.latency.setOption({
      title: { text: "平均响应时间(成功请求)", left: "center", textStyle: { fontSize: 12 } },
      tooltip: {
        trigger: "axis",
        axisPointer: {
          type: 'shadow'
        },
        formatter: '{b0}<br />{a0}: {c0} ms'
      },
      grid: { left: 40, right: 20, top: 40, bottom: 40 },
      xAxis: { type: "category", data: [] },
      yAxis: { type: "value", name: "ms" },
      series: [{ name: "平均响应时间", type: "bar", data: [] }]
    });

    this.charts.proxyUsage.setOption({
      title: { text: "代理使用统计", left: "center", textStyle: { fontSize: 12 } },
      tooltip: {
        trigger: 'axis',
        axisPointer: {
          type: 'shadow'
        }
      },
      grid: { left: 100, right: 20, top: 40, bottom: 20 },
      xAxis: { type: "value", name: "请求数", nameLocation: "middle", nameGap: 25 },
      yAxis: { type: "category", data: [] },
      series: [{ name: "请求数", type: "bar", data: [] }]
    });

    this.charts.targets.setOption({
      title: { text: "Top 目标站点(24h)", left: "center", textStyle: { fontSize: 12 } },
      tooltip: {
        trigger: 'axis',
        axisPointer: {
          type: 'shadow'
        }
      },
      grid: { left: 100, right: 20, top: 40, bottom: 20 },
      xAxis: { type: "value", name: "请求数", nameLocation: "middle", nameGap: 25 },
      yAxis: { type: "category", data: [] },
      series: [{ name: "请求数", type: "bar", data: [] }]
    });
  },

  resize () {
    Object.values(this.charts).forEach(ch => ch && ch.resize());
  },

  refresh () {
    this.loadAndUpdateOverview();
    this.loadAndUpdateHourly();
    this.loadAndUpdateProxyUsage();
    this.loadAndUpdateTargets();
    this.loadFailedTargets();
    trafficLogManager.load();
  },

  ensure () {
    const ids = ["chart-traffic", "chart-latency", "chart-proxy-usage", "chart-targets"];
    const needReinit = ids.some(id => {
      const el = document.getElementById(id);
      if (!el) return true;
      const inst = echarts.getInstanceByDom(el);
      return !inst || el.clientWidth === 0 || el.clientHeight === 0;
    });

    if (needReinit) {
      this.init();
    } else {
      requestAnimationFrame(() => {
        this.resize();
        setTimeout(() => this.resize(), 16);
        this.refresh();
      });
    }
  },

  async loadAndUpdateOverview () {
    try {
      const res = await fetch(`${API_BASE}/api/stats/overview`);
      const data = await res.json();
      this.updateOverviewKPIs(data);
    } catch (e) {
      console.error("概览数据获取失败", e);
    }
  },

  updateOverviewKPIs (data) {
    const els = {
      active: document.querySelector("#active-proxies"),
      total: document.querySelector("#total-requests"),
      failed: document.querySelector("#failed-requests"),
      rt: document.querySelector("#avg-rt")
    };

    const getNumber = (el) => Number(el.textContent.replace(/,/g, "")) || 0;

    uiUtils.animateNumber(els.active, getNumber(els.active), data.activeProxies || 0);
    uiUtils.animateNumber(els.total, getNumber(els.total), data.totalRequests || 0);
    uiUtils.animateNumber(els.failed, getNumber(els.failed), data.failedRequests || 0);
    uiUtils.animateNumber(els.rt, getNumber(els.rt), data.avgResponseTime || 0);

    const prev = this.lastOverview || {
      activeProxies: 0,
      totalRequests: 0,
      failedRequests: 0,
      avgResponseTime: 0
    };

    this.updateKPIDeltas(prev, data);
    this.lastOverview = data;
  },

  updateKPIDeltas (prev, curr) {
    const deltaText = (p, c, unit = "") => {
      if (p == null || c == null) return "";
      const diff = c - p;
      if (diff === 0) return "与上次持平";
      const sign = diff > 0 ? "+" : "";
      return `${sign}${diff}${unit}`;
    };

    document.querySelector("#kpi-total-delta").textContent = deltaText(prev.totalRequests, curr.totalRequests);
    document.querySelector("#kpi-failed-delta").textContent = deltaText(prev.failedRequests, curr.failedRequests);
    document.querySelector("#kpi-rt-delta").textContent = deltaText(prev.avgResponseTime, curr.avgResponseTime, "ms");
  },

  updateStatisticsPartial (data) {
    const totalEl = document.querySelector("#total-requests");
    const currentTotal = Number(totalEl.textContent.replace(/,/g, "")) || 0;
    uiUtils.animateNumber(totalEl, currentTotal, currentTotal + 1);

    if (!data?.success) {
      const failedEl = document.querySelector("#failed-requests");
      const currentFailed = Number(failedEl.textContent.replace(/,/g, "")) || 0;
      uiUtils.animateNumber(failedEl, currentFailed, currentFailed + 1);
    }
  },

  async loadAndUpdateHourly () {
    try {
      const res = await fetch(`${API_BASE}/api/stats/hourly`);
      const data = await res.json();
      this.applyHourlyToCharts(data);
    } catch (e) {
      console.error("小时统计获取失败", e);
    }
  },

  applyHourlyToCharts (arr) {
    if (!Array.isArray(arr)) return;

    const validEntries = arr.filter(item => {
      const rawHour = dataExtractor.getRawHour(item);
      const date = dateUtils.parse(rawHour);
      if (!date) {
        console.warn('Skipping invalid hour entry:', rawHour);
        return false;
      }
      return true;
    });

    const sorted = validEntries.sort((a, b) => {
      const da = dateUtils.parse(dataExtractor.getRawHour(a));
      const db = dateUtils.parse(dataExtractor.getRawHour(b));
      return (da?.getTime() || 0) - (db?.getTime() || 0);
    });

    // 根据格式化的小时标签删除重复内容
    const uniqueMap = new Map();
    sorted.forEach(item => {
      const label = dateUtils.formatHourLabel(dataExtractor.getRawHour(item));
      const succ = dataExtractor.getSuccess(item);
      if (!uniqueMap.has(label) || succ > dataExtractor.getSuccess(uniqueMap.get(label))) {
        uniqueMap.set(label, item);
      }
    });

    const unique = Array.from(uniqueMap.values());
    const x = unique.map(i => dateUtils.formatHourLabel(dataExtractor.getRawHour(i)));
    const succ = unique.map(i => dataExtractor.getSuccess(i));
    const fail = unique.map(i => dataExtractor.getFailed(i));
    const avg = unique.map(i => dataExtractor.getAvgRt(i));

    this.charts.traffic?.setOption(
      { xAxis: { data: x }, series: [{ data: succ }, { data: fail }] },
      false, true
    );

    this.charts.latency?.setOption(
      { xAxis: { data: x }, series: [{ data: avg }] },
      false, true
    );

    this.chartDataState.traffic = { xAxis: x, success: succ, failed: fail };
    this.chartDataState.latency = { xAxis: x, data: avg };
  },

  updateHourlyChart (hourlyData) {
    if (!Array.isArray(hourlyData)) return;
    this.applyHourlyToCharts(hourlyData);
  },

  updateChartsPartial (payload) {
    if (!payload) return;

    if (Array.isArray(payload)) {
      this.applyHourlyToCharts(payload);
      return;
    }
    if (payload.type === "hourly" || Array.isArray(payload.hourly)) {
      const data = payload.type === "hourly" ? payload.data : payload.hourly;
      this.applyHourlyToCharts(Array.isArray(data) ? data : []);
      return;
    }
    if (payload.type === "proxy_usage") {
      this.updateProxyUsageChart(Array.isArray(payload.data) ? payload.data : []);
    } else if (payload.type === "overview") {
      this.updateOverviewKPIs(payload.data || {});
    }
  },

  async loadAndUpdateProxyUsage () {
    try {
      const res = await fetch(`${API_BASE}/api/stats/proxy-usage`);
      const data = await res.json();
      this.updateProxyUsageChart(data);
    } catch (e) {
      console.error("代理使用统计获取失败", e);
    }
  },

  updateProxyUsageChart (usage) {
    if (!Array.isArray(usage)) return;

    const sorted = [...usage]
      .sort((a, b) => (b.total_requests || 0) - (a.total_requests || 0))
      .slice(0, 10);

    const y = sorted.map(i => `${i.name} (${String(i.type || "").toUpperCase()})`);
    const v = sorted.map(i => i.total_requests || 0);

    this.charts.proxyUsage?.setOption(
      {
        yAxis: { data: y },
        series: [{ data: v, animationDuration: 500, animationEasing: "cubicOut" }]
      },
      false, true
    );

    this.chartDataState.proxyUsage = { yAxis: y, data: v };
  },

  async loadAndUpdateTargets () {
    try {
      const res = await fetch(`${API_BASE}/api/stats/targets`);
      const data = await res.json();
      const top = data.slice(0, 10);
      const y = top.map(i => i.target_host);
      const v = top.map(i => i.request_count || 0);

      this.charts.targets?.setOption(
        {
          yAxis: { data: y },
          series: [{ data: v, animationDuration: 500, animationEasing: "cubicOut" }]
        },
        false, true
      );

      this.chartDataState.targets = { yAxis: y, data: v };
    } catch (e) {
      console.error("目标站点统计获取失败", e);
    }
  },

  async loadFailedTargets () {
    try {
      const response = await fetch(`${API_BASE}/api/stats/failed-targets`);
      const data = await response.json();

      const tbody = document.querySelector("#failed-targets-list");
      const noDataDiv = document.querySelector("#no-failed-targets");

      if (data.length === 0) {
        tbody.innerHTML = "";
        noDataDiv.classList.remove("hidden");
      } else {
        noDataDiv.classList.add("hidden");
        tbody.innerHTML = data.map((item, index) => `
          <tr class="border-b hover:bg-gray-50">
            <td class="py-2 px-3">
              <span class="inline-flex items-center justify-center w-6 h-6 rounded-full 
                ${index < 3 ? "bg-red-100 text-red-600 font-bold" : "bg-gray-100 text-gray-600"} text-xs">
                ${index + 1}
              </span>
            </td>
            <td class="py-2 px-3">
              <span class="font-mono text-gray-700">${item.target}</span>
            </td>
            <td class="text-center py-2 px-3">
              <span class="inline-flex items-center px-2 py-1 rounded-full text-xs font-medium bg-red-100 text-red-800">
                ${item.fail_count}
              </span>
            </td>
            <td class="text-right py-2 px-3 text-gray-500 text-xs">
              ${dateUtils.format(item.last_fail_time)}
            </td>
          </tr>
        `).join("");
      }
    } catch (error) {
      console.error("加载失败请求统计失败:", error);
    }
  }
};

const TRAFFIC_STATUS_META = Object.freeze({
  direct_success: { text: "成功", className: "bg-green-100 text-green-700" },
  health_success: { text: "健康检查成功", className: "bg-emerald-100 text-emerald-700" },
  direct_failure: { text: "代理连接失败", className: "bg-red-100 text-red-700" },
  health_failure: { text: "健康检查失败", className: "bg-amber-100 text-amber-700" },
  proxy_exhausted: { text: "无可用代理", className: "bg-rose-100 text-rose-700" },
  proxy_error: { text: "请求异常", className: "bg-orange-100 text-orange-700" },
  io_error: { text: "连接中断", className: "bg-red-100 text-red-700" }
});

const trafficLogManager = {
  maxRows: 100,
  logs: [],

  async load () {
    try {
      const response = await fetch(`${API_BASE}/api/traffic-logs`);
      if (!response.ok) {
        throw new Error(`HTTP ${response.status}`);
      }
      const data = await response.json();
      this.logs = Array.isArray(data) ? data.slice(0, this.maxRows) : [];
      this.render();
    } catch (error) {
      console.error("流量日志获取失败", error);
    }
  },

  pushRealtime (log) {
    if (!log) return;
    const normalized = this.normalizeRealtimeLog(log);
    this.logs = [normalized, ...this.logs].slice(0, this.maxRows);
    this.render();
  },

  normalizeRealtimeLog (log) {
    return {
      id: log.id || null,
      proxy_id: log.proxyId ?? null,
      proxy_name: log.proxyName || null,
      proxy_type: log.proxyType || null,
      proxy_host: log.proxyHost || null,
      proxy_port: log.proxyPort ?? null,
      target_host: log.targetHost || null,
      target_port: log.targetPort ?? null,
      success: log.success === true ? 1 : log.success === false ? 0 : log.success,
      response_time: log.responseTime ?? null,
      error_message: log.errorMessage || null,
      result_type: log.resultType || null,
      created_at: log.createdAt || new Date().toISOString()
    };
  },

  render () {
    const tbody = document.querySelector("#traffic-log-list");
    const noDataDiv = document.querySelector("#no-traffic-logs");
    if (!tbody || !noDataDiv) return;

    if (!Array.isArray(this.logs) || this.logs.length === 0) {
      tbody.innerHTML = "";
      noDataDiv.classList.remove("hidden");
      return;
    }

    noDataDiv.classList.add("hidden");
    tbody.innerHTML = this.logs.map(log => this.renderRow(log)).join("");
  },

  renderRow (log) {
    const status = this.resolveStatus(log);
    const responseTime = this.formatResponseTime(log.response_time);
    const errorMessage = this.formatError(log.error_message);
    const target = this.formatTarget(log.target_host, log.target_port);
    const proxy = this.formatProxy(log);

    return `
      <tr class="border-b hover:bg-gray-50">
        <td class="py-2 px-3 text-xs text-gray-500 whitespace-nowrap">${dateUtils.format(log.created_at)}</td>
        <td class="py-2 px-3">${target}</td>
        <td class="py-2 px-3">${proxy}</td>
        <td class="py-2 px-3 text-center">
          <span class="inline-flex items-center px-2 py-1 rounded-full text-xs font-medium ${status.className}">
            ${status.text}
          </span>
        </td>
        <td class="py-2 px-3 text-right text-xs text-gray-700 whitespace-nowrap">${responseTime}</td>
        <td class="py-2 px-3 text-xs text-red-600">${errorMessage}</td>
      </tr>
    `;
  },

  resolveStatus (log) {
    const type = String(log.result_type || "").toLowerCase();
    if (type && TRAFFIC_STATUS_META[type]) {
      return TRAFFIC_STATUS_META[type];
    }
    if (log.success === 1 || log.success === true) {
      return { text: "成功", className: "bg-green-100 text-green-700" };
    }
    if (log.success === 0 || log.success === false) {
      return { text: "失败", className: "bg-red-100 text-red-700" };
    }
    return { text: "未知", className: "bg-gray-100 text-gray-600" };
  },

  formatTarget (host, port) {
    const hostText = host ? escapeHtml(host) : "-";
    const portText = (port || port === 0) ? `:${port}` : "";
    return `<span class="font-mono text-gray-700">${hostText}${portText}</span>`;
  },

  formatProxy (log) {
    const proxyName = log.proxy_name || "";
    const proxyType = log.proxy_type ? String(log.proxy_type).toUpperCase() : "";
    const proxyHost = log.proxy_host || "";
    const proxyPort = (log.proxy_port || log.proxy_port === 0) ? String(log.proxy_port) : "";

    if (!proxyName && !proxyType && !proxyHost && !proxyPort) {
      return `<span class="text-gray-400">未使用代理</span>`;
    }

    const endpoint = proxyHost && proxyPort ? `${proxyHost}:${proxyPort}` : "";
    const nameLine = proxyName || `代理#${log.proxy_id ?? "-"}`;
    const typeLine = proxyType ? `${proxyType}${endpoint ? ` · ${endpoint}` : ""}` : endpoint;

    return `
      <div class="min-w-0">
        <div class="font-medium text-gray-800">${escapeHtml(nameLine)}</div>
        <div class="text-xs text-gray-500">${escapeHtml(typeLine || "-")}</div>
      </div>
    `;
  },

  formatResponseTime (value) {
    const number = Number(value);
    if (!Number.isFinite(number) || number <= 0) {
      return "<span class=\"text-gray-400\">-</span>";
    }
    return `${Math.round(number)}ms`;
  },

  formatError (message) {
    if (!message) {
      return "<span class=\"text-gray-400\">-</span>";
    }
    const raw = String(message);
    const short = raw.length > 80 ? `${raw.slice(0, 80)}...` : raw;
    return `<span title="${escapeHtml(raw)}">${escapeHtml(short)}</span>`;
  }
};

// 页面可见性监听
document.addEventListener('visibilitychange', () => {
  wsManager.setPageActive(!document.hidden);
  if (!document.hidden && uiUtils.isSystemStatusVisible()) {
    chartManager.ensure();
  }
});

// 页面切换
function showSection (sectionId, el) {
  document.querySelectorAll(".section").forEach(s => s.classList.add("hidden"));
  document.getElementById(sectionId).classList.remove("hidden");

  const items = document.querySelectorAll(".sidebar-item");
  items.forEach(item => item.classList.remove("active"));

  if (el && el.classList) {
    el.classList.add("active");
  } else {
    items.forEach(item => {
      const code = item.getAttribute("onclick") || "";
      if (code.includes(`'${sectionId}'`)) item.classList.add("active");
    });
  }

  const titles = {
    "proxy-list": "代理配置管理",
    "dns-mappings": "DNS映射管理",
    "test-settings": "负载设置",
    "advanced-config": "高级配置",
    "system-status": "系统状态"
  };
  document.querySelector("#page-title").textContent = titles[sectionId] || "";

  if (sectionId === "system-status") {
    if (!window.__systemStatusInited) {
      window.__systemStatusInited = true;
      chartManager.init();
    } else {
      chartManager.ensure();
    }
  } else if (sectionId === "dns-mappings") {
    loadDNSMappings();
  } else if (sectionId === "advanced-config") {
    loadAdvancedConfig();
  } else if (sectionId === "test-settings") {
    loadProxyGroups();
  }
}

// 优先级管理
async function updateSinglePriority (proxyId, priority) {
  try {
    const response = await fetch(`${API_BASE}/api/proxies/${proxyId}/priority`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ priority: parseInt(priority) })
    });
    if (response.ok) await loadPriorityList();
  } catch (error) {
    console.error("更新优先级失败:", error);
  }
}

async function adjustPriority (proxyId, currentPriority) {
  const { value: newPriority } = await Swal.fire({
    title: "调整优先级",
    input: "number",
    inputLabel: "输入新的优先级（数字越小越优先）",
    inputValue: currentPriority || 999,
    showCancelButton: true,
    confirmButtonText: "确定",
    cancelButtonText: "取消",
    inputAttributes: { min: 1, max: 999, step: 1 }
  });

  if (newPriority) {
    try {
      const response = await fetch(`${API_BASE}/api/proxies/${proxyId}/priority`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ priority: parseInt(newPriority) })
      });

      if (response.ok) {
        uiUtils.showToast("优先级已更新", "success");
        proxyManager.loadList();
      }
    } catch (error) {
      Swal.fire({ icon: "error", title: "更新失败", text: error.message });
    }
  }
}

async function togglePriorityMode (persist = true) {
  const mode = document.querySelector("#load-mode").value;
  window.currentLoadMode = mode;

  const panel = document.querySelector("#priority-panel");
  if (mode === "manual") {
    panel.classList.remove("hidden");
    await loadPriorityList();
  } else {
    panel.classList.add("hidden");
  }

  if (persist) {
    await fetch(`${API_BASE}/api/settings`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ load_mode: mode })
    });
  }

  proxyManager.loadList();
}

async function loadPriorityList () {
  const response = await fetch(`${API_BASE}/api/proxies`);
  const proxies = await response.json();

  proxies.sort((a, b) => (a.priority || 999) - (b.priority || 999));

  const container = document.querySelector("#priority-list");
  container.innerHTML = proxies.map((proxy, index) => `
    <div class="priority-item flex items-center p-3 bg-gray-50 rounded cursor-move hover:bg-gray-100 transition-colors" 
        data-proxy-id="${proxy.id}" draggable="true">
      <i class="fas fa-grip-vertical text-gray-400 mr-3"></i>
      <span class="priority-number text-lg font-bold text-gray-400 mr-4 w-8">${index + 1}</span>
      <div class="flex-1">
        <span class="font-medium">${proxy.name}</span>
        <span class="text-sm text-gray-500 ml-2">${proxy.type.toUpperCase()} - ${proxy.host}:${proxy.port}</span>
      </div>
      <input type="number" value="${index + 1}" 
        class="w-20 px-2 py-1 border rounded text-center priority-input"
        min="1" max="999" onchange="updateSinglePriority(${proxy.id}, this.value)">
    </div>
  `).join("");

  initDragAndDrop();
}

// 拖拽排序
function initDragAndDrop () {
  const container = document.querySelector("#priority-list");
  let draggedElement = null;
  let placeholder = null;

  function createPlaceholder () {
    const ph = document.createElement("div");
    ph.className = "h-14 bg-blue-100 border-2 border-dashed border-blue-300 rounded transition-all";
    return ph;
  }

  container.querySelectorAll(".priority-item").forEach(item => {
    item.addEventListener("dragstart", e => {
      draggedElement = item;
      placeholder = createPlaceholder();
      item.classList.add("dragging", "opacity-50");
      e.dataTransfer.effectAllowed = "move";
      setTimeout(() => {
        item.style.display = "none";
        item.parentNode.insertBefore(placeholder, item);
      }, 0);
    });

    item.addEventListener("dragend", () => {
      if (draggedElement) {
        draggedElement.style.display = "";
        draggedElement.classList.remove("dragging", "opacity-50");
        if (placeholder && placeholder.parentNode) {
          placeholder.parentNode.replaceChild(draggedElement, placeholder);
        }
        updatePriorityNumbers();
        draggedElement = null;
        placeholder = null;
      }
    });
  });

  container.addEventListener("dragover", e => {
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";

    if (draggedElement && placeholder) {
      const afterElement = getDragAfterElement(container, e.clientY);
      if (afterElement == null) {
        container.appendChild(placeholder);
      } else {
        container.insertBefore(placeholder, afterElement);
      }
    }
  });

  container.addEventListener("drop", e => {
    e.preventDefault();
    if (draggedElement && placeholder) {
      placeholder.parentNode.replaceChild(draggedElement, placeholder);
      updatePriorityNumbers();
      savePriorities();
    }
  });
}

function getDragAfterElement (container, y) {
  const draggableElements = [...container.querySelectorAll(".priority-item:not(.dragging)")];
  return draggableElements.reduce((closest, child) => {
    const box = child.getBoundingClientRect();
    const offset = y - box.top - box.height / 2;
    if (offset < 0 && offset > closest.offset) return { offset, element: child };
    return closest;
  }, { offset: Number.NEGATIVE_INFINITY }).element;
}

function updatePriorityNumbers () {
  const items = document.querySelectorAll("#priority-list .priority-item");
  items.forEach((item, index) => {
    const numberSpan = item.querySelector(".priority-number");
    const input = item.querySelector(".priority-input");
    if (numberSpan) numberSpan.textContent = index + 1;
    if (input) input.value = index + 1;
  });
}

async function savePriorities () {
  const items = document.querySelectorAll("#priority-list .priority-item");
  const priorities = {};
  items.forEach((item, index) => {
    const proxyId = item.dataset.proxyId;
    priorities[proxyId] = index + 1;
  });

  try {
    const response = await fetch(`${API_BASE}/api/proxies/priorities`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ priorities })
    });

    if (response.ok) {
      uiUtils.showToast("优先级已更新", "success");
      proxyManager.loadList();
    }
  } catch (error) {
    uiUtils.showToast("保存失败", "error");
  }
}

// 刷新功能
function refreshProxyList () {
  if (uiUtils.isSystemStatusVisible()) {
    uiUtils.showToast("正在刷新统计数据...", "info");
    chartManager.refresh();
  } else {
    proxyManager.loadList();
  }
}

// 设置管理
async function loadSettings () {
  const response = await fetch(`${API_BASE}/api/settings`);
  const settings = await response.json();

  if (settings.test_url) {
    document.querySelector("#test-url").value = settings.test_url;
  }
  if (settings.timeout) {
    document.querySelector("#test-timeout").value = settings.timeout;
  }
  if (settings.load_mode) {
    document.querySelector("#load-mode").value = settings.load_mode;
    window.currentLoadMode = settings.load_mode;
    togglePriorityMode(false);
  }
  if (settings.algorithm) {
    const algorithmSelect = document.querySelector("#algorithm");
    const exists = Array.from(algorithmSelect.options).some(opt => opt.value === settings.algorithm);
    algorithmSelect.value = exists ? settings.algorithm : "adaptive";
  }

  // 加载高级配置
  await loadAdvancedConfig();
}

async function saveTestSettings () {
  const testUrl = document.querySelector("#test-url").value;
  const timeout = parseInt(document.querySelector("#test-timeout").value, 10);
  const algorithm = document.querySelector("#algorithm").value;

  try {
    const response = await fetch(`${API_BASE}/api/settings`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ test_url: testUrl, timeout, algorithm })
    });

    if (response.ok) {
      uiUtils.showToast("设置保存成功", "success");
    }
  } catch (error) {
    uiUtils.showToast("保存失败", "error");
  }
}

// 代理模态框管理
async function showAddProxyModal () {
  document.querySelector("#modal-title").textContent = "添加代理配置";
  document.querySelector("#proxy-id").value = "";
  document.querySelector("#proxy-name").value = "";
  document.querySelector("#proxy-type").value = "http";
  document.querySelector("#proxy-host").value = "";
  document.querySelector("#host-port").value = "";
  document.querySelector("#proxy-username").value = "";
  document.querySelector("#proxy-password").value = "";
  document.querySelector("#proxy-enabled").checked = true;
  document.querySelector("#proxy-test-url").value = "";
  document.querySelector("#proxy-test-timeout").value = "";
  await loadTestUrlOptions();
  document.querySelector("#proxy-modal").classList.remove("hidden");
}

async function loadTestUrlOptions () {
  try {
    const response = await fetch(`${API_BASE}/api/test-urls`);
    const urls = await response.json();
    const datalist = document.querySelector("#test-url-options");
    datalist.innerHTML = urls.map(url => `<option value="${url}">`).join("");
  } catch (e) {
    console.error("加载测试地址列表失败:", e);
  }
}

function closeProxyModal () {
  document.querySelector("#proxy-modal").classList.add("hidden");
}

async function saveProxy () {
  const id = document.querySelector("#proxy-id").value;
  const testTimeoutVal = document.querySelector("#proxy-test-timeout").value;
  const data = {
    name: document.querySelector("#proxy-name").value,
    type: document.querySelector("#proxy-type").value,
    host: document.querySelector("#proxy-host").value,
    port: parseInt(document.querySelector("#host-port").value),
    username: document.querySelector("#proxy-username").value,
    password: document.querySelector("#proxy-password").value,
    enabled: document.querySelector("#proxy-enabled").checked ? 1 : 0,
    test_url: document.querySelector("#proxy-test-url").value || null,
    test_timeout: testTimeoutVal ? parseInt(testTimeoutVal) : null
  };

  if (!data.name || !data.host || !data.port) {
    Swal.fire({ icon: "warning", title: "请填写必要信息" });
    return;
  }

  try {
    const url = id ? `${API_BASE}/api/proxies/${id}` : `${API_BASE}/api/proxies`;
    const method = id ? "PUT" : "POST";

    const response = await fetch(url, {
      method,
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(data)
    });

    if (response.ok) {
      closeProxyModal();
      await Swal.fire({ icon: "success", title: "保存成功" });
      proxyManager.loadList();
    }
  } catch (error) {
    Swal.fire({ icon: "error", title: "保存失败", text: error.message });
  }
}

async function editProxy (id) {
  try {
    const response = await fetch(`${API_BASE}/api/proxies/${id}`);
    const proxy = await response.json();

    document.querySelector("#modal-title").textContent = "编辑代理配置";
    document.querySelector("#proxy-id").value = proxy.id;
    document.querySelector("#proxy-name").value = proxy.name;
    document.querySelector("#proxy-type").value = proxy.type;
    document.querySelector("#proxy-host").value = proxy.host;
    document.querySelector("#host-port").value = proxy.port;
    document.querySelector("#proxy-username").value = proxy.username || "";
    document.querySelector("#proxy-password").value = proxy.password || "";
    document.querySelector("#proxy-enabled").checked = proxy.enabled === 1 || proxy.enabled === true;
    document.querySelector("#proxy-test-url").value = proxy.test_url || "";
    document.querySelector("#proxy-test-timeout").value = proxy.test_timeout || "";

    await loadTestUrlOptions();
    toggleAuthFields();
    document.querySelector("#proxy-modal").classList.remove("hidden");
  } catch (error) {
    console.log(error.message);
    Swal.fire({ icon: "info", title: "加载代理信息失败" });
  }
}

async function deleteProxy (id) {
  const res = await Swal.fire({
    title: "确定要删除这个代理配置吗？",
    icon: "warning",
    showCancelButton: true,
    confirmButtonText: "删除",
    cancelButtonText: "取消"
  });

  if (!res.isConfirmed) return;

  try {
    const response = await fetch(`${API_BASE}/api/proxies/${id}`, { method: "DELETE" });
    if (response.ok) {
      await Swal.fire({ icon: "success", title: "删除成功" });
      proxyManager.loadList();
    } else {
      const t = await response.text();
      await Swal.fire({ icon: "error", title: "删除失败", text: t || "" });
    }
  } catch (error) {
    await Swal.fire({ icon: "error", title: "删除失败", text: error.message });
  }
}

function getBatchProgressHtml (progress) {
  return `
    <div class="text-sm text-gray-600">正在测试已启用代理...</div>
    <div class="mt-2">进度: <b>${progress.completed}</b> / ${progress.total}</div>
    <div class="mt-1 text-xs text-gray-500">成功 ${progress.success} / 失败 ${progress.failed}</div>
  `;
}

function updateBatchProgress (progress) {
  if (!Swal.isVisible()) return;
  Swal.update({ html: getBatchProgressHtml(progress) });
}

async function testAllProxies () {
  if (batchTestState.running) return;

  let proxies = proxyManager.allProxies;
  if (!Array.isArray(proxies) || proxies.length === 0) {
    try {
      const response = await fetch(`${API_BASE}/api/proxies`);
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      proxies = await response.json();
      proxyManager.allProxies = proxies;
    } catch (error) {
      await Swal.fire({ icon: "error", title: "批量测试失败", text: error.message });
      return;
    }
  }

  const targets = proxies.filter(proxy => (proxy.enabled === 1 || proxy.enabled === true) && !testingButtons.has(String(proxy.id)));

  if (targets.length === 0) {
    await Swal.fire({
      icon: "info",
      title: "没有可测试的代理",
      text: "当前没有启用中的代理"
    });
    return;
  }

  batchTestState.running = true;
  const batchToken = createTestToken("batch");
  batchTestState.token = batchToken;
  setBatchTestButtonState(true);

  const progress = { total: targets.length, completed: 0, success: 0, failed: 0 };
  try {
    Swal.fire({
      title: "一键连通性测试",
      html: getBatchProgressHtml(progress),
      allowOutsideClick: false,
      showConfirmButton: false,
      didOpen: () => Swal.showLoading()
    });

    const queue = targets.slice();
    const concurrency = Math.min(5, queue.length);
    const workers = Array.from({ length: concurrency }).map(async () => {
      while (queue.length > 0) {
        const proxy = queue.shift();
        if (!proxy) break;
        const proxyKey = String(proxy.id);
        if (testingButtons.has(proxyKey)) {
          progress.completed += 1;
          updateBatchProgress(progress);
          continue;
        }

        const token = `${batchToken}-${proxy.id}`;
        setProxyTestingState(proxy.id, token);
        try {
          const response = await fetch(`${API_BASE}/api/proxies/${proxy.id}/test`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ measureBandwidth: false })
          });
          if (!response.ok) throw new Error(`HTTP ${response.status}`);
          const result = await response.json();
          if (result.success) {
            progress.success += 1;
          } else {
            progress.failed += 1;
          }
        } catch (error) {
          progress.failed += 1;
        } finally {
          clearProxyTestingState(proxy.id, token);
          progress.completed += 1;
          updateBatchProgress(progress);
        }
      }
    });

    await Promise.all(workers);
  } finally {
    batchTestState.running = false;
    batchTestState.token = null;
    setBatchTestButtonState(false);
  }

  if (Swal.isVisible()) {
    Swal.close();
  }

  await Swal.fire({
    icon: progress.failed > 0 ? "warning" : "success",
    title: "批量测试完成",
    html: `成功 <b>${progress.success}</b> / ${progress.total}，失败 <b>${progress.failed}</b>`,
    confirmButtonText: "知道了"
  });

  proxyManager.loadList();
}

async function testProxy (event, id, opts = {}) {
  const proxyKey = String(id);
  if (testingButtons.has(proxyKey)) return;

  const token = createTestToken("single");
  setProxyTestingState(proxyKey, token);
  let shouldRefresh = false;

  try {
    const proxyResponse = await fetch(`${API_BASE}/api/proxies/${id}`);
    if (!proxyResponse.ok) throw new Error(`HTTP ${proxyResponse.status}`);
    const proxy = await proxyResponse.json();

    if (!proxy.enabled || proxy.enabled === 0) {
      await Swal.fire({
        icon: "warning",
        title: "代理未启用",
        text: "请先启用该代理再进行测试"
      });
      return;
    }

    const response = await fetch(`${API_BASE}/api/proxies/${id}/test`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ measureBandwidth: false })
    });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const result = await response.json();
    shouldRefresh = true;

    if (result.success) {
      await Swal.fire({
        icon: "success",
        title: "连通性测试成功",
        html: `<div>连通/握手耗时：<b>${result.responseTime} ms</b></div>`
      });
    } else {
      await Swal.fire({
        icon: "info",
        title: `测试失败: ${result.error || "未知错误"}`
      });
    }
  } catch (error) {
    await Swal.fire({ icon: "error", title: "测试失败", text: error.message });
  } finally {
    clearProxyTestingState(proxyKey, token);
  }

  if (shouldRefresh) {
    proxyManager.loadList();
  }
}

function toggleAuthFields () {
  const proxyType = document.querySelector("#proxy-type").value;
  const authFields = document.querySelector("#auth-fields");
  authFields.style.display = proxyType === "socks4" ? "none" : "grid";
}

// 加载高级配置
async function loadAdvancedConfig () {
  try {
    const response = await fetch(`${API_BASE}/api/advanced-config`);
    const config = await response.json();

    // 基础配置
    if (config.proxy_port) document.querySelector("#proxy-port").value = config.proxy_port;
    if (config.periodic_test_interval) document.querySelector("#periodic-test-interval").value = config.periodic_test_interval / 60000; // 转换为分钟
    if (config.log_retention_days) document.querySelector("#log-retention-days").value = config.log_retention_days;
    if (config.stats_retention_days) document.querySelector("#stats-retention-days").value = config.stats_retention_days;

    // 连接池配置
    if (config.pool_max_size) document.querySelector("#pool-max-size").value = config.pool_max_size;
    if (config.pool_idle_timeout) document.querySelector("#pool-idle-timeout").value = config.pool_idle_timeout / 1000; // 转换为秒
    if (config.pool_wait_timeout) document.querySelector("#pool-wait-timeout").value = config.pool_wait_timeout / 1000;

    // 熔断器配置
    if (config.circuit_failure_threshold) document.querySelector("#circuit-failure-threshold").value = config.circuit_failure_threshold;
    if (config.circuit_timeout) document.querySelector("#circuit-timeout").value = config.circuit_timeout / 1000; // 转换为秒
    if (config.circuit_half_open_attempts) document.querySelector("#circuit-half-open-attempts").value = config.circuit_half_open_attempts;

    // 健康检查配置
    if (config.health_check_interval) document.querySelector("#health-check-interval").value = config.health_check_interval / 1000;
    if (config.health_degrade_threshold) document.querySelector("#health-degrade-threshold").value = config.health_degrade_threshold;
    if (config.health_recover_threshold) document.querySelector("#health-recover-threshold").value = config.health_recover_threshold;

    // 快速失败配置
    if (config.failfast_enabled !== undefined) document.querySelector("#failfast-enabled").checked = config.failfast_enabled;
    if (config.failfast_max_attempts) document.querySelector("#failfast-max-attempts").value = config.failfast_max_attempts;
    if (config.failfast_attempt_timeout) document.querySelector("#failfast-attempt-timeout").value = config.failfast_attempt_timeout;
    if (config.failfast_total_timeout) document.querySelector("#failfast-total-timeout").value = config.failfast_total_timeout;

    // 算法权重配置
    if (config.algorithm_weights) {
      const weights = config.algorithm_weights;
      if (weights.responseTime !== undefined) {
        document.querySelector("#weight-response").value = weights.responseTime * 100;
        document.querySelector("#weight-response-value").textContent = weights.responseTime.toFixed(2);
      }
      if (weights.successRate !== undefined) {
        document.querySelector("#weight-success").value = weights.successRate * 100;
        document.querySelector("#weight-success-value").textContent = weights.successRate.toFixed(2);
      }
      if (weights.bandwidth !== undefined) {
        document.querySelector("#weight-bandwidth").value = weights.bandwidth * 100;
        document.querySelector("#weight-bandwidth-value").textContent = weights.bandwidth.toFixed(2);
      }
      if (weights.connections !== undefined) {
        document.querySelector("#weight-connections").value = weights.connections * 100;
        document.querySelector("#weight-connections-value").textContent = weights.connections.toFixed(2);
      }
      if (weights.stability !== undefined) {
        document.querySelector("#weight-stability").value = weights.stability * 100;
        document.querySelector("#weight-stability-value").textContent = weights.stability.toFixed(2);
      }
      if (weights.recentPerf !== undefined) {
        document.querySelector("#weight-recent").value = weights.recentPerf * 100;
        document.querySelector("#weight-recent-value").textContent = weights.recentPerf.toFixed(2);
      }
    }

    updateWeightTotal();

    // 更新显示的端口号
    document.querySelector("#proxy-port-display").textContent = config.proxy_port || 5678;

  } catch (error) {
    console.error("加载高级配置失败:", error);
  }
}

// 保存高级配置
async function saveAdvancedConfig () {
  const config = {
    // 基础配置
    proxy_port: parseInt(document.querySelector("#proxy-port").value),
    periodic_test_interval: parseInt(document.querySelector("#periodic-test-interval").value) * 60000, // 转换为毫秒
    log_retention_days: parseInt(document.querySelector("#log-retention-days").value),
    stats_retention_days: parseInt(document.querySelector("#stats-retention-days").value),

    // 连接池配置
    pool_max_size: parseInt(document.querySelector("#pool-max-size").value),
    pool_idle_timeout: parseInt(document.querySelector("#pool-idle-timeout").value) * 1000, // 转换为毫秒
    pool_wait_timeout: parseInt(document.querySelector("#pool-wait-timeout").value) * 1000,

    // 熔断器配置
    circuit_failure_threshold: parseInt(document.querySelector("#circuit-failure-threshold").value),
    circuit_timeout: parseInt(document.querySelector("#circuit-timeout").value) * 1000, // 转换为毫秒
    circuit_half_open_attempts: parseInt(document.querySelector("#circuit-half-open-attempts").value),

    // 健康检查配置
    health_check_interval: parseInt(document.querySelector("#health-check-interval").value) * 1000,
    health_degrade_threshold: parseFloat(document.querySelector("#health-degrade-threshold").value),
    health_recover_threshold: parseFloat(document.querySelector("#health-recover-threshold").value),

    // 快速失败配置
    failfast_enabled: document.querySelector("#failfast-enabled").checked,
    failfast_max_attempts: parseInt(document.querySelector("#failfast-max-attempts").value),
    failfast_attempt_timeout: parseInt(document.querySelector("#failfast-attempt-timeout").value),
    failfast_total_timeout: parseInt(document.querySelector("#failfast-total-timeout").value),

    // 算法权重配置
    algorithm_weights: {
      responseTime: parseFloat(document.querySelector("#weight-response").value) / 100,
      successRate: parseFloat(document.querySelector("#weight-success").value) / 100,
      bandwidth: parseFloat(document.querySelector("#weight-bandwidth").value) / 100,
      connections: parseFloat(document.querySelector("#weight-connections").value) / 100,
      stability: parseFloat(document.querySelector("#weight-stability").value) / 100,
      recentPerf: parseFloat(document.querySelector("#weight-recent").value) / 100
    }
  };

  try {
    const response = await fetch(`${API_BASE}/api/advanced-config`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(config)
    });

    if (response.ok) {
      const result = await response.json();

      if (result.requiresRestart) {
        await Swal.fire({
          icon: "success",
          title: "配置已保存",
          text: "部分配置需要重启服务才能生效",
          showCancelButton: true,
          confirmButtonText: "立即重启",
          cancelButtonText: "稍后重启"
        }).then((result) => {
          if (result.isConfirmed) {
            // 可以添加重启服务的逻辑
            window.location.reload();
          }
        });
      } else {
        uiUtils.showToast("配置保存成功", "success");
      }

      // 更新显示的端口号
      document.querySelector("#proxy-port-display").textContent = config.proxy_port;
    }
  } catch (error) {
    Swal.fire({ icon: "error", title: "保存失败", text: error.message });
  }
}

// 重置为默认配置
async function resetAdvancedConfig () {
  const result = await Swal.fire({
    title: "确定恢复默认配置？",
    text: "这将重置所有高级配置为默认值",
    icon: "warning",
    showCancelButton: true,
    confirmButtonText: "确定恢复",
    cancelButtonText: "取消"
  });

  if (result.isConfirmed) {
    try {
      const response = await fetch(`${API_BASE}/api/advanced-config/reset`, {
        method: "POST"
      });

      if (response.ok) {
        await loadAdvancedConfig();
        uiUtils.showToast("已恢复默认配置", "success");
      }
    } catch (error) {
      Swal.fire({ icon: "error", title: "恢复失败", text: error.message });
    }
  }
}

// 导出配置
async function exportConfig () {
  try {
    const response = await fetch(`${API_BASE}/api/advanced-config/export`);
    const config = await response.json();

    const dataStr = JSON.stringify(config, null, 2);
    const dataUri = 'data:application/json;charset=utf-8,' + encodeURIComponent(dataStr);

    const exportFileDefaultName = `proxy-config-${new Date().toISOString().split('T')[0]}.json`;

    const linkElement = document.createElement('a');
    linkElement.setAttribute('href', dataUri);
    linkElement.setAttribute('download', exportFileDefaultName);
    linkElement.click();

    uiUtils.showToast("配置已导出", "success");
  } catch (error) {
    Swal.fire({ icon: "error", title: "导出失败", text: error.message });
  }
}

// 更新权重显示
function updateWeightDisplay (name) {
  const input = document.querySelector(`#weight-${name}`);
  const display = document.querySelector(`#weight-${name}-value`);
  const value = parseFloat(input.value) / 100;
  display.textContent = value.toFixed(2);
  updateWeightTotal();
}

// 更新权重总和
function updateWeightTotal () {
  const weights = [
    'response', 'success', 'bandwidth',
    'connections', 'stability', 'recent'
  ];

  let total = 0;
  weights.forEach(name => {
    const value = parseFloat(document.querySelector(`#weight-${name}`).value) / 100;
    total += value;
  });

  const totalDisplay = document.querySelector("#weight-total");
  totalDisplay.textContent = total.toFixed(2);

  if (Math.abs(total - 1.0) > 0.01) {
    totalDisplay.className = "text-red-600";
  } else {
    totalDisplay.className = "text-green-600";
  }
}

// 加载版本信息
async function loadVersionInfo() {
  try {
    const response = await fetch(`${API_BASE}/api/version`);
    const versionInfo = await response.json();

    // 显示版本号
    const versionDisplay = document.querySelector("#version-display");
    if (versionDisplay) {
      versionDisplay.textContent = `v${versionInfo.version}`;
      versionDisplay.title = `构建时间: ${new Date(versionInfo.buildTime).toLocaleString('zh-CN')}\n环境: ${versionInfo.environment}\nNode: ${versionInfo.nodeVersion}`;
    }
  } catch (error) {
    console.error("加载版本信息失败:", error);
    const versionDisplay = document.querySelector("#version-display");
    if (versionDisplay) {
      versionDisplay.textContent = "未知";
    }
  }
}

// ==================== DNS映射管理 ====================

// 加载DNS映射列表
async function loadDNSMappings() {
  try {
    const response = await fetch(`${API_BASE}/api/dns-mappings`);
    const mappings = await response.json();

    const tbody = document.querySelector("#dns-table-body");
    if (!tbody) return;

    if (mappings.length === 0) {
      tbody.innerHTML = `
        <tr>
          <td colspan="5" class="text-center py-8 text-gray-500">
            <i class="fas fa-inbox text-4xl mb-2"></i>
            <p>暂无DNS映射</p>
          </td>
        </tr>
      `;
      return;
    }

    tbody.innerHTML = mappings.map(mapping => `
      <tr class="border-b hover:bg-gray-50">
        <td class="py-3 px-4">
          <div class="font-medium">${escapeHtml(mapping.domain)}</div>
        </td>
        <td class="py-3 px-4">
          <code class="text-sm bg-gray-100 px-2 py-1 rounded">${escapeHtml(mapping.ip)}</code>
        </td>
        <td class="py-3 px-4">
          <span class="text-sm text-gray-600">${escapeHtml(mapping.description || '-')}</span>
        </td>
        <td class="py-3 px-4 text-center">
          <span class="px-2 py-1 text-xs rounded-full ${mapping.enabled ? 'bg-green-100 text-green-800' : 'bg-gray-100 text-gray-800'}">
            ${mapping.enabled ? '已启用' : '已禁用'}
          </span>
        </td>
        <td class="py-3 px-4 text-center">
          <button onclick="editDNSMapping(${mapping.id})" class="text-blue-600 hover:text-blue-800 mr-2" title="编辑">
            <i class="fas fa-edit"></i>
          </button>
          <button onclick="toggleDNSMapping(${mapping.id})" class="text-${mapping.enabled ? 'orange' : 'green'}-600 hover:text-${mapping.enabled ? 'orange' : 'green'}-800 mr-2" title="${mapping.enabled ? '禁用' : '启用'}">
            <i class="fas fa-${mapping.enabled ? 'pause' : 'play'}-circle"></i>
          </button>
          <button onclick="deleteDNSMapping(${mapping.id})" class="text-red-600 hover:text-red-800" title="删除">
            <i class="fas fa-trash"></i>
          </button>
        </td>
      </tr>
    `).join('');
  } catch (error) {
    console.error("加载DNS映射失败:", error);
    Swal.fire({ icon: "error", title: "加载失败", text: "加载DNS映射失败" });
  }
}

// 显示添加DNS映射模态框
function showAddDNSModal() {
  document.querySelector("#dns-modal-title").textContent = "新增DNS映射";
  document.querySelector("#dns-id").value = "";
  document.querySelector("#dns-domain").value = "";
  document.querySelector("#dns-ip").value = "";
  document.querySelector("#dns-description").value = "";
  document.querySelector("#dns-enabled").checked = true;
  document.querySelector("#dns-modal").classList.remove("hidden");
}

// 编辑DNS映射
async function editDNSMapping(id) {
  try {
    const response = await fetch(`${API_BASE}/api/dns-mappings`);
    const mappings = await response.json();
    const mapping = mappings.find(m => m.id === id);

    if (!mapping) {
      Swal.fire({ icon: "error", title: "错误", text: "DNS映射不存在" });
      return;
    }

    document.querySelector("#dns-modal-title").textContent = "编辑DNS映射";
    document.querySelector("#dns-id").value = mapping.id;
    document.querySelector("#dns-domain").value = mapping.domain;
    document.querySelector("#dns-ip").value = mapping.ip;
    document.querySelector("#dns-description").value = mapping.description || "";
    document.querySelector("#dns-enabled").checked = mapping.enabled === 1;
    document.querySelector("#dns-modal").classList.remove("hidden");
  } catch (error) {
    console.error("加载DNS映射失败:", error);
    Swal.fire({ icon: "error", title: "加载失败", text: "加载DNS映射失败" });
  }
}

// 关闭DNS映射模态框
function closeDNSModal() {
  document.querySelector("#dns-modal").classList.add("hidden");
}

// 保存DNS映射
async function saveDNS() {
  const id = document.querySelector("#dns-id").value;
  const domain = document.querySelector("#dns-domain").value.trim();
  const ip = document.querySelector("#dns-ip").value.trim();
  const description = document.querySelector("#dns-description").value.trim();
  const enabled = document.querySelector("#dns-enabled").checked ? 1 : 0;

  if (!domain || !ip) {
    Swal.fire({ icon: "warning", title: "提示", text: "域名和IP地址不能为空" });
    return;
  }

  // 验证IP格式
  const ipRegex = /^(\d{1,3}\.){3}\d{1,3}$/;
  if (!ipRegex.test(ip)) {
    Swal.fire({ icon: "warning", title: "提示", text: "IP地址格式不正确" });
    return;
  }

  try {
    const url = id ? `${API_BASE}/api/dns-mappings/${id}` : `${API_BASE}/api/dns-mappings`;
    const method = id ? "PUT" : "POST";

    const response = await fetch(url, {
      method,
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ domain, ip, description, enabled })
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(error.error || "保存失败");
    }

    await Swal.fire({ icon: "success", title: "成功", text: id ? "DNS映射已更新" : "DNS映射已添加" });
    closeDNSModal();
    loadDNSMappings();
  } catch (error) {
    console.error("保存DNS映射失败:", error);
    Swal.fire({ icon: "error", title: "保存失败", text: error.message || "保存DNS映射失败" });
  }
}

// 切换DNS映射启用状态
async function toggleDNSMapping(id) {
  try {
    const response = await fetch(`${API_BASE}/api/dns-mappings/${id}/toggle`, {
      method: "PUT"
    });

    if (!response.ok) {
      throw new Error("切换状态失败");
    }

    const result = await response.json();
    await Swal.fire({
      icon: "success",
      title: "成功",
      text: result.enabled ? "DNS映射已启用" : "DNS映射已禁用",
      timer: 1500,
      showConfirmButton: false
    });
    loadDNSMappings();
  } catch (error) {
    console.error("切换DNS映射状态失败:", error);
    Swal.fire({ icon: "error", title: "失败", text: "切换状态失败" });
  }
}

// 删除DNS映射
async function deleteDNSMapping(id) {
  const result = await Swal.fire({
    title: "确认删除",
    text: "确定要删除这个DNS映射吗？",
    icon: "warning",
    showCancelButton: true,
    confirmButtonColor: "#d33",
    cancelButtonColor: "#3085d6",
    confirmButtonText: "删除",
    cancelButtonText: "取消"
  });

  if (!result.isConfirmed) return;

  try {
    const response = await fetch(`${API_BASE}/api/dns-mappings/${id}`, {
      method: "DELETE"
    });

    if (!response.ok) {
      throw new Error("删除失败");
    }

    await Swal.fire({
      icon: "success",
      title: "成功",
      text: "DNS映射已删除",
      timer: 1500,
      showConfirmButton: false
    });
    loadDNSMappings();
  } catch (error) {
    console.error("删除DNS映射失败:", error);
    Swal.fire({ icon: "error", title: "删除失败", text: error.message || "删除DNS映射失败" });
  }
}

// HTML转义函数
function escapeHtml(text) {
  if (!text) return '';
  const div = document.createElement('div');
  div.textContent = text;
  return div.innerHTML;
}

// ==================== 代理分组管理 ====================

let groupDomains = [];

async function loadProxyGroups () {
  try {
    const response = await fetch(`${API_BASE}/api/proxy-groups`);
    const groups = await response.json();
    renderProxyGroups(groups);
  } catch (error) {
    console.error("加载代理分组失败:", error);
  }
}

function renderProxyGroups (groups) {
  const container = document.querySelector("#proxy-groups-list");
  const noGroups = document.querySelector("#no-proxy-groups");

  if (!groups || groups.length === 0) {
    container.innerHTML = "";
    noGroups.classList.remove("hidden");
    return;
  }

  noGroups.classList.add("hidden");
  container.innerHTML = groups.map(group => `
    <div class="border rounded-lg p-4 hover:shadow-md transition-shadow ${group.is_default ? 'border-blue-300 bg-blue-50' : ''}">
      <div class="flex items-center justify-between mb-2">
        <div class="flex items-center gap-2">
          <h4 class="font-semibold text-gray-800">${escapeHtml(group.name)}</h4>
          ${group.is_default ? '<span class="text-xs bg-blue-500 text-white px-2 py-0.5 rounded-full">默认</span>' : ''}
          ${!group.enabled ? '<span class="text-xs bg-gray-400 text-white px-2 py-0.5 rounded-full">已禁用</span>' : ''}
        </div>
        <div class="flex items-center gap-2">
          <button onclick="editGroup(${group.id})" class="text-blue-500 hover:text-blue-700 text-sm"><i class="fas fa-edit"></i></button>
          <button onclick="deleteGroup(${group.id})" class="text-red-500 hover:text-red-700 text-sm"><i class="fas fa-trash"></i></button>
        </div>
      </div>
      <div class="mb-2">
        <span class="text-xs text-gray-500 mr-1">域名:</span>
        ${group.domains.length > 0
          ? group.domains.map(d => `<span class="inline-block text-xs bg-gray-100 text-gray-700 px-2 py-0.5 rounded mr-1 mb-1">${escapeHtml(d.domain)}</span>`).join("")
          : '<span class="text-xs text-gray-400">无</span>'}
      </div>
      <div>
        <span class="text-xs text-gray-500 mr-1">代理:</span>
        ${group.members.length > 0
          ? group.members.map(m => `<span class="inline-block text-xs ${m.status === 'active' ? 'bg-green-100 text-green-700' : 'bg-gray-100 text-gray-700'} px-2 py-0.5 rounded mr-1 mb-1">${escapeHtml(m.name)}</span>`).join("")
          : '<span class="text-xs text-gray-400">无</span>'}
      </div>
    </div>
  `).join("");
}

async function showGroupModal () {
  document.querySelector("#group-modal-title").textContent = "新增代理分组";
  document.querySelector("#group-id").value = "";
  document.querySelector("#group-name").value = "";
  document.querySelector("#group-is-default").checked = false;
  groupDomains = [];
  renderGroupDomainTags();
  await loadGroupProxyCheckboxes();
  document.querySelector("#group-modal").classList.remove("hidden");
}

function closeGroupModal () {
  document.querySelector("#group-modal").classList.add("hidden");
}

async function loadGroupProxyCheckboxes (selectedIds = []) {
  try {
    const response = await fetch(`${API_BASE}/api/proxies`);
    const data = await response.json();
    const proxies = data.proxies || data;
    const container = document.querySelector("#group-proxy-checkboxes");
    container.innerHTML = proxies.map(p => `
      <label class="flex items-center gap-2 p-2 hover:bg-gray-50 rounded cursor-pointer">
        <input type="checkbox" value="${p.id}" class="group-proxy-cb rounded" ${selectedIds.includes(p.id) ? 'checked' : ''} />
        <span class="text-sm">${escapeHtml(p.name)}</span>
        <span class="text-xs text-gray-400">${p.type}://${p.host}:${p.port}</span>
        <span class="text-xs ml-auto ${p.status === 'active' ? 'text-green-500' : 'text-gray-400'}">${p.status === 'active' ? '在线' : p.status || '未知'}</span>
      </label>
    `).join("");
  } catch (e) {
    console.error("加载代理列表失败:", e);
  }
}

function addGroupDomain () {
  const input = document.querySelector("#group-domain-input");
  const domain = input.value.trim().toLowerCase();
  if (!domain) return;
  if (groupDomains.includes(domain)) {
    input.value = "";
    return;
  }
  groupDomains.push(domain);
  input.value = "";
  renderGroupDomainTags();
}

function removeGroupDomain (domain) {
  groupDomains = groupDomains.filter(d => d !== domain);
  renderGroupDomainTags();
}

function renderGroupDomainTags () {
  const container = document.querySelector("#group-domains-tags");
  container.innerHTML = groupDomains.map(d => `
    <span class="inline-flex items-center gap-1 bg-blue-100 text-blue-700 text-sm px-2 py-1 rounded">
      ${escapeHtml(d)}
      <button type="button" onclick="removeGroupDomain('${d}')" class="text-blue-500 hover:text-blue-700">&times;</button>
    </span>
  `).join("");
}

async function saveGroup () {
  const id = document.querySelector("#group-id").value;
  const name = document.querySelector("#group-name").value.trim();
  if (!name) {
    Swal.fire({ icon: "warning", title: "请填写分组名称" });
    return;
  }

  const proxyIds = [...document.querySelectorAll(".group-proxy-cb:checked")].map(cb => parseInt(cb.value));
  const data = {
    name,
    domains: groupDomains,
    proxy_ids: proxyIds,
    is_default: document.querySelector("#group-is-default").checked ? 1 : 0,
    enabled: 1
  };

  try {
    const url = id ? `${API_BASE}/api/proxy-groups/${id}` : `${API_BASE}/api/proxy-groups`;
    const method = id ? "PUT" : "POST";
    const response = await fetch(url, {
      method,
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(data)
    });

    if (response.ok) {
      closeGroupModal();
      await Swal.fire({ icon: "success", title: "保存成功", timer: 1500, showConfirmButton: false });
      loadProxyGroups();
    } else {
      const err = await response.json();
      Swal.fire({ icon: "error", title: "保存失败", text: err.error || "未知错误" });
    }
  } catch (error) {
    Swal.fire({ icon: "error", title: "保存失败", text: error.message });
  }
}

async function editGroup (id) {
  try {
    const response = await fetch(`${API_BASE}/api/proxy-groups`);
    const groups = await response.json();
    const group = groups.find(g => g.id === id);
    if (!group) return;

    document.querySelector("#group-modal-title").textContent = "编辑代理分组";
    document.querySelector("#group-id").value = group.id;
    document.querySelector("#group-name").value = group.name;
    document.querySelector("#group-is-default").checked = !!group.is_default;
    groupDomains = group.domains.map(d => d.domain);
    renderGroupDomainTags();
    await loadGroupProxyCheckboxes(group.members.map(m => m.proxy_id));
    document.querySelector("#group-modal").classList.remove("hidden");
  } catch (error) {
    Swal.fire({ icon: "error", title: "加载分组信息失败", text: error.message });
  }
}

async function deleteGroup (id) {
  const res = await Swal.fire({
    title: "确定要删除这个分组吗？",
    icon: "warning",
    showCancelButton: true,
    confirmButtonColor: "#d33",
    confirmButtonText: "确定删除",
    cancelButtonText: "取消"
  });

  if (!res.isConfirmed) return;

  try {
    const response = await fetch(`${API_BASE}/api/proxy-groups/${id}`, { method: "DELETE" });
    if (response.ok) {
      await Swal.fire({ icon: "success", title: "删除成功", timer: 1500, showConfirmButton: false });
      loadProxyGroups();
    }
  } catch (error) {
    Swal.fire({ icon: "error", title: "删除失败", text: error.message });
  }
}

// ==================== 初始化 ====================

// 初始化
document.addEventListener("DOMContentLoaded", () => {
  // 渲染页面框架
  proxyManager.renderSkeleton();
  loadSettings();
  loadVersionInfo();
  loadProxyGroups();

  // 使用 requestIdleCallback 延迟非关键操作
  if ('requestIdleCallback' in window) {
    requestIdleCallback(() => {
      wsManager.connect();
      proxyManager.loadList();
    }, { timeout: 1000 });
  } else {
    // 降级方案
    setTimeout(() => {
      wsManager.connect();
      proxyManager.loadList();
    }, 100);
  }
});

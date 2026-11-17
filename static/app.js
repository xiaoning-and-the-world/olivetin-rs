// @ts-check
// OliveTin-RS 前端逻辑：动作列表、执行、版本管理、上传（进度条）、日志、设置。
// 零构建渐进类型化：本文件仍是唯一前端产物（Rust include_str! 内联），
// 契约类型见 static/types.d.ts，检查命令见 target/verify-patch/check_types.js。
(function () {
  'use strict';

  const API = {
    actions: '/api/actions',
    logs: '/api/logs',
  };

  // 设置页占用刷新：模块级单例，renderSettings 重建时先清旧定时器。
  let usageTimer = null;
  let usageRefresh = null;
  // 只注册一次的全局可见性监听：切回页面立即补一次刷新。
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden && usageRefresh) usageRefresh();
  });
  // dialog 的 close 事件不冒泡，用捕获阶段统一移除节点，
  // 避免反复开弹窗后 DOM 节点、闭包与重复 id 单调累积。
  document.addEventListener('close', (ev) => {
    if (ev.target && ev.target.tagName === 'DIALOG') ev.target.remove();
  }, true);
  // 详情页子命令菜单的 document 级监听用 AbortController 去重。
  let detailClickAbort = null;
  // renderActions 全量重建：每次丢弃全部旧节点，不做局部 diff。
  // 状态外置到下面两个 Map，重建后按 key 恢复；AbortController 统一注销上一轮监听器。
  let actionsAbort = null;
  let actionsGeneration = 0;
  const groupCollapsedState = new Map();
  const paramDrafts = new Map();
  function bind(el, type, fn) {
    el.addEventListener(type, fn, actionsAbort ? { signal: actionsAbort.signal } : {});
  }

  // 界面设置缓存：改动设置后置空，下次进入动作页重新拉取
  /** @type {SettingsResponse | null} */
  let uiSettings = null;
  async function loadUiSettings() {
    if (uiSettings) return uiSettings;
    try {
      /** @type {SettingsResponse} */
      const data = await fetchJSON('/api/settings');
      uiSettings = data;
    } catch (e) {
      // 失败时不缓存空对象，下次进入动作页会重试。
      uiSettings = null;
    }
    return uiSettings || {};
  }

  function toast(msg) {
    const el = document.getElementById('toast');
    if (!el) {
      const t = document.createElement('div');
      t.id = 'toast';
      document.body.appendChild(t);
    }
    const t = document.getElementById('toast');
    t.textContent = msg;
    t.classList.add('show');
    clearTimeout(t._timer);
    t._timer = setTimeout(() => t.classList.remove('show'), 3000);
  }

  // 统一请求错误类型（后端 Q8/Q9 方案 B）：HTTP 状态码为主，
  // message 仅由响应体尽力解析；status / body 原样保留，现有调用点无需改动。
  class HttpError extends Error {
    constructor(status, body, fallback) {
      /** @type {any} */
      const b = body && typeof body === 'object' ? body : {};
      /** @type {any} */
      const raw = b.error !== undefined ? b.error : b.message;
      super(typeof raw === 'string' && raw ? raw : (fallback || '请求失败'));
      this.name = 'HttpError';
      this.status = status;
      this.body = b;
    }
  }

  /**
   * 统一请求封装。后端响应由 handler.rs 的 json! 宏构造、没有可生成类型的 Schema，
   * 因此返回 any 是有意为之；关键调用点用 @type 标注具体契约类型。
   * @param {string} url
   * @param {RequestInit} [opts]
   * @returns {Promise<any>}
   */
  async function fetchJSON(url, opts) {
    const resp = await fetch(url, opts);
    if (resp.status === 401) {
      toast('登录已过期，请重新访问带 token 的链接');
      throw new HttpError(401, {}, '登录已过期，请重新访问带 token 的链接');
    }
    if (resp.status === 429) {
      toast('请求过于频繁');
      throw new HttpError(429, {}, '请求过于频繁');
    }
    if (resp.status === 403) {
      toast('没有权限或请求被安全策略拒绝（403），请重新确认登录状态');
      throw new HttpError(403, {}, '没有权限或请求被安全策略拒绝（403）');
    }
    if (!resp.ok) {
      let body = {};
      try { body = await resp.json(); } catch (_) { console.warn('fetchJSON: 错误响应体非 JSON'); }
      throw new HttpError(resp.status, body, '请求失败');
    }
    return resp.json();
  }

  // ============ 通用工具 ============

  function escapeHtml(s) {
    return String(s === undefined || s === null ? '' : s)
      .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
  }

  // sh 脚本文件名规则（与后端 config::Action::script_filename 一致）：
  // 留空 → {id}.sh；已指定 → 不以 .sh 结尾时自动补 .sh。
  function normalizeScriptName(raw, id) {
    const v = (raw || '').trim();
    if (!v) return id + '.sh';
    return v.endsWith('.sh') ? v : v + '.sh';
  }

  function scriptFullPath(dir, raw, id) {
    return String(dir || '').replace(/\/$/, '') + '/' + normalizeScriptName(raw, id);
  }

  // ===== 服务器（路由器）上的脚本：读取 / 浏览 =====
  // 面板服务本身跑在路由器上，这两个接口读的是它的本地磁盘，
  // 因此可以直接用「路由器上的路径」，不必先把文件弄到电脑上。

  // 后端路径校验为唯一权威（后端 Q4/Q5）：这里只做非阻断的格式提示，
  // 集中放在两个请求封装内，自动覆盖全部 6 个调用点；不拦截、不镜像白名单。
  function hintRemotePath(value, label) {
    const v = String(value || '').trim();
    if (!v) return;
    const issues = [];
    if (!v.startsWith('/')) issues.push('不是绝对路径');
    if (/(^|\/)\.\.(\/|$)/.test(v)) issues.push('含上层路径 ..');
    if (/\s/.test(v)) issues.push('含空白字符');
    if (issues.length) {
      toast(label + '格式提示：' + issues.join('、') + '；是否可读取由服务端决定');
    }
  }

  async function inspectRemoteScript(path) {
    hintRemotePath(path, '脚本路径');
    return fetchJSON('/api/scripts/inspect', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ path }),
    });
  }

  async function listRemoteScripts(dir) {
    hintRemotePath(dir, '目录');
    return fetchJSON('/api/scripts/list', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ dir: dir || '' }),
    });
  }

  function humanSize(n) {
    if (!n && n !== 0) return '';
    if (n < 1024) return n + ' B';
    if (n < 1024 * 1024) return (n / 1024).toFixed(1) + ' KB';
    return (n / 1024 / 1024).toFixed(1) + ' MB';
  }

  // 弹出「选择服务器脚本」列表：点一行回调路径。路径可手改目录再列出。
  function openServerScriptPicker(startDir, onPick) {
    const dlg = document.createElement('dialog');
    dlg.innerHTML =
      '<div class="dialog-header"><span class="dialog-title">选择服务器上的脚本</span>' +
      '<button class="close-btn" data-close>×</button></div>' +
      '<div class="field"><label>目录（路由器上的绝对路径）</label>' +
      '<input type="text" id="pk-dir" class="mono" value="' + escapeHtml(startDir || '') + '"></div>' +
      '<div style="display:flex;gap:8px;margin-bottom:10px;">' +
      '<button class="secondary" id="pk-go" style="font-size:12px;padding:4px 10px;">列出该目录的脚本</button>' +
      '</div>' +
      '<div id="pk-list" class="muted" style="font-size:12px;max-height:46vh;overflow:auto;">加载中…</div>';
    document.body.appendChild(dlg);
    dlg.showModal();
    dlg.querySelector('[data-close]').addEventListener('click', () => dlg.close());

    const listBox = dlg.querySelector('#pk-list');
    const dirInput = dlg.querySelector('#pk-dir');
    const load = async () => {
      listBox.textContent = '加载中…';
      try {
        const d = await listRemoteScripts(dirInput.value.trim());
        dirInput.value = d.dir || dirInput.value;
        const items = d.items || [];
        if (!items.length) {
          listBox.textContent = '该目录下没有 .sh / .bash 文件';
          return;
        }
        listBox.textContent = '';
        const frag = document.createDocumentFragment();
        items.forEach((it) => {
          const row = document.createElement('div');
          row.style.cssText = 'padding:8px;border-bottom:1px solid var(--border);cursor:pointer;';
          row.innerHTML = '<div class="mono" style="font-size:12px;word-break:break-all;">' + escapeHtml(it.path) + '</div>' +
            '<div style="font-size:11px;color:var(--text-muted);">' + humanSize(it.size) +
            (it.executable ? ' · 可执行' : ' · 无执行位') + '</div>';
          row.addEventListener('click', () => {
            dlg.close();
            onPick(it.path);
          });
          frag.appendChild(row);
        });
        listBox.appendChild(frag);
      } catch (e) {
        listBox.textContent = (e.body && (e.body.error || e.body.message)) || '读取目录失败';
      }
    };
    dlg.querySelector('#pk-go').addEventListener('click', load);
    load();
  }

  // 复制文本：HTTP 局域网访问不是安全上下文，navigator.clipboard 不存在，
  // 此时回退到临时 textarea + execCommand('copy')，否则点击「复制」会静默失败。
  async function copyText(text) {
    if (navigator.clipboard && window.isSecureContext) {
      try {
        await navigator.clipboard.writeText(text);
        return true;
      } catch (e) { /* 落到下面的兼容路径 */ }
    }
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.setAttribute('readonly', '');
    ta.style.cssText = 'position:fixed;top:0;left:-9999px;opacity:0;';
    document.body.appendChild(ta);
    ta.focus();
    ta.select();
    ta.setSelectionRange(0, ta.value.length);
    let ok = false;
    try { ok = document.execCommand('copy'); } catch (e) { ok = false; }
    document.body.removeChild(ta);
    return ok;
  }

  // ============ 动作列表 ============
  async function loadActions() {
    const root = document.getElementById('actions-root');
    if (!root) return;
    try {
      const [data, ui] = await Promise.all([fetchJSON(API.actions), loadUiSettings()]);
      renderActions(root, data, ui || {});
    } catch (e) {
      root.innerHTML = '<div class="muted">加载失败</div>';
    }
  }

  function renderActions(root, data, ui) {
    // 全量重建：注销上一轮监听器 → 丢弃全部旧节点 → 从最新数据生成全部节点。
    // 不留任何旧 DOM 引用，折叠状态与参数草稿由模块级状态恢复。
    if (actionsAbort) actionsAbort.abort();
    actionsAbort = new AbortController();
    const gen = ++actionsGeneration;
    const scrollY = window.scrollY;
    root.replaceChildren();
    const header = document.createElement('div');
    header.style.cssText = 'display:flex;justify-content:space-between;align-items:center;flex-wrap:wrap;gap:8px;margin-bottom:12px;';
    const title = document.createElement('h1');
    title.textContent = '动作';
    title.style.margin = '0';
    header.appendChild(title);
    const btnGroup = document.createElement('div');
    btnGroup.style.cssText = 'display:flex;gap:8px;';
    const groupBtn = document.createElement('button');
    groupBtn.className = 'secondary';
    groupBtn.textContent = '分组管理';
    bind(groupBtn, 'click', openGroupManager);
    btnGroup.appendChild(groupBtn);
    const addBtn = document.createElement('button');
    addBtn.textContent = '+ 新增脚本';
    bind(addBtn, 'click', openCreateActionDialog);
    btnGroup.appendChild(addBtn);
    header.appendChild(btnGroup);
    root.appendChild(header);

    const groups = data.groups || [];
    const position = (ui.group_panel_position === 'left' || ui.group_panel_position === 'right')
      ? ui.group_panel_position : 'top';
    const horizontal = ui.group_layout === 'horizontal';

    // 分组面板位置：top=原有纵向堆叠；left/right=侧栏导航 + 内容区
    const layout = document.createElement('div');
    layout.className = 'groups-layout pos-' + position;
    const nav = document.createElement('div');
    nav.className = 'group-nav';
    const content = document.createElement('div');
    content.className = 'groups-content';

    groups.forEach((group, idx) => {
      // 每个分组独立记忆折叠状态；未记忆过才用设置页默认值。
      const groupKey = group.name;
      const collapsed = groupCollapsedState.has(groupKey)
        ? groupCollapsedState.get(groupKey)
        : !!ui.collapse_groups_by_default;
      if (position !== 'top') {
        const b = document.createElement('button');
        b.className = 'secondary group-nav-item';
        b.textContent = group.name + ' (' + (group.actions || []).length + ')';
        bind(b, 'click', () => {
          const target = content.children[idx];
          if (!target) return;
          target.querySelector('.group-body').classList.remove('collapsed');
          target.querySelector('.group-header').classList.remove('collapsed');
          target.scrollIntoView({ behavior: 'smooth', block: 'start' });
        });
        nav.appendChild(b);
      }

      const g = document.createElement('div');
      g.className = 'group';
      g.dataset.group = group.name;
      const gh = document.createElement('div');
      gh.className = 'group-header' + (collapsed ? ' collapsed' : '');
      gh.textContent = group.name;
      bind(gh, 'click', () => {
        const body = g.querySelector('.group-body');
        const isCollapsed = body.classList.toggle('collapsed');
        gh.classList.toggle('collapsed', isCollapsed);
        groupCollapsedState.set(groupKey, isCollapsed);
      });
      g.appendChild(gh);
      const body = document.createElement('div');
      body.className = 'group-body ' + (horizontal ? 'layout-horizontal' : 'layout-vertical')
        + (collapsed ? ' collapsed' : '');
      group.actions.forEach((a) => body.appendChild(actionCard(a, gen)));
      g.appendChild(body);
      content.appendChild(g);
    });

    if (position !== 'top') layout.appendChild(nav);
    layout.appendChild(content);
    root.appendChild(layout);
    // 全量重建后恢复滚动位置；折叠状态与参数草稿已在构建时按 key 恢复。
    window.scrollTo(0, scrollY);
  }

  function actionCard(a, gen) {
    const card = document.createElement('div');
    card.className = 'card';
    card.dataset.id = a.id;
    card.style.cursor = 'pointer';
    // 整卡点击进入详情页（点击按钮/输入框时不触发）
    bind(card, 'click', (ev) => {
      const t = ev.target;
      if (t.closest('button') || t.closest('input') || t.closest('select') || t.closest('details') || t.closest('a')) return;
      window.location.href = '/action/' + encodeURIComponent(a.id);
    });

    // 标题行：固定单行（标题过长省略号），避免「标题 + 定时标签 + 按钮」换行
    // 导致同样结构的卡片高度不一样。
    const titleRow = document.createElement('div');
    titleRow.className = 'card-title-row';
    const title = document.createElement('span');
    title.className = 'card-title';
    title.textContent = a.title;
    title.title = a.title;
    title.style.cursor = 'pointer';
    bind(title, 'click', () => { window.location.href = '/action/' + encodeURIComponent(a.id); });
    titleRow.appendChild(title);

    const right = document.createElement('span');
    right.className = 'card-tools';
    if (a.active_version) {
      const badge = document.createElement('span');
      badge.className = 'badge';
      badge.textContent = a.active_version;
      badge.title = '版本管理';
      badge.style.cursor = 'pointer';
      bind(badge, 'click', (ev) => {
        ev.stopPropagation();  // 阻止冒泡到卡片，避免触发整卡点击跳详情页
        openVersionPanel(a);
      });
      right.appendChild(badge);
    }
    if (a.schedule && a.schedule.enabled) {
      const tag = document.createElement('span');
      tag.className = 'tag';
      tag.textContent = '🕐 ' + a.schedule.description;
      tag.title = a.schedule.description || '';
      right.appendChild(tag);
    }
    // 编辑按钮
    const editBtn = document.createElement('button');
    editBtn.className = 'secondary';
    editBtn.textContent = '编辑';
    editBtn.style.cssText = 'font-size:12px;padding:3px 10px;';
    bind(editBtn, 'click', () => openEditActionDialog(a.id));
    right.appendChild(editBtn);
    // 删除按钮
    const delBtn = document.createElement('button');
    delBtn.className = 'danger';
    delBtn.textContent = '删除';
    delBtn.style.cssText = 'font-size:12px;padding:3px 10px;';
    bind(delBtn, 'click', () => deleteAction(a));
    right.appendChild(delBtn);
    titleRow.appendChild(right);
    card.appendChild(titleRow);

    // 描述槽位恒存在（无描述时留空一行），卡片内容结构因此完全一致
    const d = document.createElement('div');
    d.className = 'card-desc';
    d.textContent = a.description || '';
    card.appendChild(d);

    // 定时槽位恒存在：有定时显示下次触发，没有则显示「手动触发」
    const m = document.createElement('div');
    m.className = 'meta-line card-meta';
    if (a.schedule && a.schedule.enabled && a.schedule.next_run_human) {
      m.textContent = '下次触发：' + a.schedule.next_run_human;
    } else {
      m.textContent = '手动触发';
    }
    card.appendChild(m);

    // 参数表单
    const form = document.createElement('div');
    (a.args || []).forEach((p) => {
      const field = document.createElement('div');
      field.className = 'field';
      const label = document.createElement('label');
      label.textContent = p.description || p.name;
      if (p.required) label.classList.add('required');
      field.appendChild(label);

      // 凭据类参数不落盘；其余只存会话级 sessionStorage，标签页关闭即清理。
      const sensitiveArg = /(pass|pwd|token|secret|apikey|api_key|credential|密码|口令|密钥|凭据)/i.test(p.name || '');
      const argStoreKey = 'olivetin-arg-' + a.id + '-' + p.name;
      const remembered = sensitiveArg ? null : sessionStorage.getItem(argStoreKey);
      const val = remembered !== null ? remembered : (p.default || '');

      let input;
      if (p.options && p.options.length > 0) {
        // 有选项列表：渲染下拉框
        input = document.createElement('select');
        p.options.forEach((opt) => {
          const o = document.createElement('option');
          o.value = opt; o.textContent = opt;
          if (opt === val) o.selected = true;
          input.appendChild(o);
        });
      } else if (p.type === 'bool' || p.style === 'flag') {
        input = document.createElement('input');
        input.type = 'checkbox';
        input.checked = val === 'true' || val === '1';
      } else if (p.type === 'int') {
        input = document.createElement('input');
        input.type = 'number';
        input.value = val;
      } else {
        input = document.createElement('input');
        input.type = 'text';
        input.value = val;
      }
      input.dataset.name = p.name;
      const draftKey = a.id + ':' + p.name;
      if (paramDrafts.has(draftKey)) {
        if (input.type === 'checkbox') input.checked = paramDrafts.get(draftKey) === 'true';
        else input.value = paramDrafts.get(draftKey);
      }
      const saveDraft = () => paramDrafts.set(
        draftKey, input.type === 'checkbox' ? String(input.checked) : input.value);
      bind(input, 'input', saveDraft);
      bind(input, 'change', () => {
        if (!sensitiveArg) sessionStorage.setItem(argStoreKey, input.value);
        saveDraft();
      });
      field.appendChild(input);
      form.appendChild(field);
    });

    card.appendChild(form);

    // 执行按钮
    const execRow = document.createElement('div');
    execRow.className = 'exec-row';
    const runBtn = document.createElement('button');
    runBtn.textContent = '执行';
    bind(runBtn, 'click', async () => {
      runBtn.disabled = true;
      runBtn.textContent = '执行中…';
      const args = {};
      (a.params || []).forEach((p) => {
        const el = form.querySelector('[data-name="' + p.name + '"]');
        if (el) {
          if (el.type === 'checkbox') args[p.name] = el.checked ? 'true' : 'false';
          else args[p.name] = el.value;
        }
      });
      try {
        await fetchJSON('/api/actions/' + a.id + '/run', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ args }),
        });
        toast('已开始执行');
      } catch (e) {
        if (e.status === 409) toast('该动作正在运行中');
        else if (e.status === 429) toast('请求过于频繁');
        else toast('执行失败');
        runBtn.disabled = false;
        runBtn.textContent = '执行';
        return;
      }
      // 轮询日志，把最新结果渲染到卡片（不跳转）
      const startedAt = Date.now();
      let busy = false;
      let timer = setInterval(async () => {
        // 页面已被新一次全量重建替换：旧轮询立即退出，避免写回旧节点。
        if (gen !== actionsGeneration || !document.body.contains(card)) {
          clearInterval(timer);
          return;
        }
        if (document.hidden || busy) return;
        if (Date.now() - startedAt > 300000) {
          clearInterval(timer);
          runBtn.disabled = false;
          runBtn.textContent = '执行';
          toast('执行状态轮询超时，请手动刷新');
          return;
        }
        busy = true;
        try {
          /** @type {LogsResponse} */
          const d = await fetchJSON('/api/logs?limit=1&action_id=' + encodeURIComponent(a.id));
          const latest = d.items && d.items[0];
          if (latest) {
            renderResult(card, latest);
          }
          const cur = await fetchJSON('/api/actions/' + a.id);
          if (cur.state !== 'running') {
            clearInterval(timer);
            runBtn.disabled = false;
            runBtn.textContent = '执行';
          }
        } catch (err) {
          clearInterval(timer);
          runBtn.disabled = false;
          runBtn.textContent = '执行';
        } finally {
          busy = false;
        }
      }, 1000);
      window.addEventListener('pagehide', () => clearInterval(timer), { once: true });
    });
    execRow.appendChild(runBtn);
    card.appendChild(execRow);

    return card;
  }

  function renderResult(card, entry) {
    let old = card.querySelector('.result');
    if (old) old.remove();
    const r = document.createElement('div');
    r.className = 'result';
    const summary = document.createElement('div');
    summary.className = 'summary';
    const parts = [];
    parts.push(entry.started_at_local || entry.started_at);
    if (entry.action_version) parts.push(entry.action_version);
    parts.push('退出码 ' + (entry.exit_code === null ? '无' : entry.exit_code));
    parts.push(entry.duration_ms + 'ms');
    if (entry.timed_out) parts.push('超时');
    if (entry.truncated) parts.push('已截断');
    summary.textContent = parts.join(' · ');
    r.appendChild(summary);

    // 执行的是哪个脚本文件：直接显示，避免只能猜「命令到底落到哪里了」
    if (entry.script_path) {
      const sp = document.createElement('div');
      sp.className = 'mono muted';
      sp.style.cssText = 'font-size:12px;word-break:break-all;';
      sp.textContent = '脚本：' + entry.script_path;
      r.appendChild(sp);
    }

    if (entry.truncated) {
      const w = document.createElement('div');
      w.className = 'warn';
      w.textContent = '⚠ 输出已截断';
      r.appendChild(w);
    }
    const pre = document.createElement('pre');
    pre.textContent = (entry.stdout || '') + (entry.stderr ? '\n--- stderr ---\n' + entry.stderr : '');
    r.appendChild(pre);
    card.appendChild(r);
  }

  // ============ 版本面板 ============
  async function openVersionPanel(a) {
    const dialog = document.createElement('dialog');
    dialog.innerHTML =
      '<div class="dialog-header"><span class="dialog-title">' + escapeHtml(a.title) + ' · 版本管理</span>' +
      '<button class="close-btn" data-close>×</button></div>' +
      '<div class="version-actions" style="margin-bottom:12px;"></div>' +
      '<div class="version-list">加载中…</div>' +
      '<div class="version-rollback" style="margin-top:12px;"></div>';
    document.body.appendChild(dialog);
    dialog.showModal();
    dialog.querySelector('[data-close]').addEventListener('click', () => dialog.close());

    // 顶部按钮区：上传新版本 + 每个配置文件一个「编辑」按钮
    const actionsRow = dialog.querySelector('.version-actions');
    const cfgHolder = document.createElement('span');
    cfgHolder.style.cssText = 'display:flex;gap:8px;flex-wrap:wrap;';
    actionsRow.appendChild(cfgHolder);

    const renderConfigButtons = (data) => {
      cfgHolder.innerHTML = '';
      const names = (data && data.config_files) || [];
      if (!names.length) {
        const b = document.createElement('button');
        b.className = 'secondary';
        b.textContent = '编辑配置';
        b.addEventListener('click', () => openConfigDialog(a.id));
        cfgHolder.appendChild(b);
        return;
      }
      names.forEach((name) => {
        const b = document.createElement('button');
        b.className = 'secondary';
        b.textContent = '编辑 ' + name;
        b.title = '编辑当前版本目录里的 ' + name;
        b.addEventListener('click', () => openConfigDialog(a.id, name));
        cfgHolder.appendChild(b);
      });
    };

    const refresh = async () => {
      try {
        const data = await fetchJSON('/api/actions/' + a.id + '/versions');
        renderVersionList(dialog, a, data);
        renderConfigButtons(data);
      } catch (e) {
        dialog.querySelector('.version-list').textContent = '加载失败';
      }
    };

    if (a.upload_enabled) {
      const uploadBtn = document.createElement('button');
      uploadBtn.textContent = '上传新版本';
      uploadBtn.addEventListener('click', () => openUploadDialog(a, refresh));
      actionsRow.insertBefore(uploadBtn, cfgHolder);
    }

    await refresh();
  }

  // ============ 编辑脚本配置（按版本目录 + 按文件） ============
  async function openConfigDialog(actionId, fileName) {
    let data;
    const qs = fileName ? '?file=' + encodeURIComponent(fileName) : '';
    try {
      data = await fetchJSON('/api/actions/' + actionId + '/config' + qs);
    } catch (e) {
      toast('配置文件不存在或该脚本未开启版本管理');
      return;
    }
    const editing = data.config_name || fileName || '配置';
    // hint 会拼进 innerHTML，文件名属不可信数据，必须完整转义。
    const editingHtml = escapeHtml(editing);
    const count = (data.config_files || []).length;
    const hint = count > 1
      ? '当前版本共 ' + count + ' 个配置文件；保存只修改当前版本里的 ' + editingHtml + '，其它版本不受影响。'
      : '保存只修改当前版本里的 ' + editingHtml + '，其它版本不受影响。';
    const dialog = document.createElement('dialog');
    dialog.innerHTML =
      '<div class="dialog-header"><span class="dialog-title">编辑 ' + escapeHtml(editing) + ' · 版本 ' + escapeHtml(data.version || '') + '</span>' +
      '<button class="close-btn" data-close>×</button></div>' +
      '<div class="muted" style="font-size:12px;margin-bottom:8px;">' + hint + '</div>' +
      '<div class="field"><textarea id="cfg-text" style="width:100%;height:240px;font-family:ui-monospace,monospace;font-size:13px;padding:8px;border:1px solid var(--border);border-radius:6px;background:var(--card);color:var(--text)">' +
      escapeHtml(data.content || '') +
      '</textarea></div>' +
      '<div class="version-actions">' +
      '<button class="secondary" data-cancel>取消</button>' +
      '<button id="cfg-save">保存</button>' +
      '</div>';
    document.body.appendChild(dialog);
    dialog.showModal();
    dialog.querySelector('[data-close]').addEventListener('click', () => dialog.close());
    dialog.querySelector('[data-cancel]').addEventListener('click', () => dialog.close());
    dialog.querySelector('#cfg-save').addEventListener('click', async () => {
      const content = dialog.querySelector('#cfg-text').value;
      if (!editing || /[\\/]/.test(editing) || editing === '.' || editing === '..') {
        toast('配置文件名不合法，请重新打开编辑弹窗');
        return;
      }
      try {
        await fetchJSON('/api/actions/' + encodeURIComponent(actionId) + '/config', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ content, file: editing }),
        });
        toast('已保存到版本 ' + (data.version || '') + ' 的 ' + editing);
        dialog.close();
      } catch (e) {
        toast('保存失败：' + ((e.body && e.body.error) || '请重试'));
      }
    });
  }

  function renderVersionList(dialog, a, data) {
    const list = dialog.querySelector('.version-list');
    list.innerHTML = '';
    if (data.max_versions) {
      list.appendChild(el('div', { class: 'meta-line' },
        '版本 ' + (data.versions || []).length + ' / 上限 ' + data.max_versions +
        '（上传新版本超出上限时自动清理最旧版本）'));
    }
    const versionFrag = document.createDocumentFragment();
    (data.versions || []).forEach((v) => {
      const row = document.createElement('div');
      row.className = 'version-row';
      const name = document.createElement('span');
      name.className = 'version-name';
      name.textContent = v.name;
      row.appendChild(name);
      if (v.is_active) {
        const active = document.createElement('span');
        active.className = 'version-active';
        active.textContent = '● 活跃';
        row.appendChild(active);
      }
      const meta = document.createElement('span');
      meta.className = 'version-meta';
      const size = (v.binary_size / 1024 / 1024).toFixed(1) + ' MB';
      const source = v.uploaded_by ? '由 ' + v.uploaded_by + ' 上传' : '手动上传';
      const cfgDesc = (v.config_files && v.config_files.length)
        ? '配置 ' + v.config_files.length + ' 个: ' + v.config_files.join(', ')
        : (v.config_present ? '含配置' : '无配置');
      meta.textContent = v.mtime + ' · ' + size + ' · ' + cfgDesc + ' · ' + source;
      row.appendChild(meta);
      versionFrag.appendChild(row);

      if (!v.is_active) {
        row.addEventListener('click', () => {
          if (confirm('切换到版本 ' + v.name + '？')) {
            fetchJSON('/api/actions/' + encodeURIComponent(a.id) + '/versions/' + encodeURIComponent(v.name) + '/activate', { method: 'POST' })
              .then(() => { toast('已切换到 ' + v.name); loadActions(); dialog.close(); })
              .catch((e) => { if (!e || e.status !== 403) toast('切换失败'); });
          }
        });
      }
    });
    list.appendChild(versionFrag);

    // 回退按钮：仅当存在可回退的上一版本时显示（独立区域，避免重复添加）
    const rollbackArea = dialog.querySelector('.version-rollback');
    rollbackArea.innerHTML = '';
    if (data.has_previous) {
      const rollbackBtn = document.createElement('button');
      rollbackBtn.className = 'danger';
      rollbackBtn.textContent = '回退到上一版本';
      rollbackBtn.addEventListener('click', () => {
        if (confirm('回退到上一版本？')) {
          fetchJSON('/api/actions/' + a.id + '/versions/rollback', { method: 'POST' })
            .then((resp) => { toast('已回退到 ' + resp.active); loadActions(); dialog.close(); })
            .catch(() => toast('回退失败'));
        }
      });
      rollbackArea.appendChild(rollbackBtn);
    }
  }

  // ============ 上传弹窗 ============
  function openUploadDialog(a, onDone) {
    const dialog = document.createElement('dialog');
    dialog.innerHTML =
      '<div class="dialog-header"><span class="dialog-title">上传新版本</span>' +
      '<button class="close-btn" data-close>×</button></div>' +
      '<div class="field"><label>版本号</label><input type="text" id="up-version" value="自动递增（v0.1 → v0.2 → …）" readonly></div>' +
      '<div class="field"><label>程序文件（可执行文件或带 shebang 的 .sh）</label><input type="file" id="up-binary"></div>' +
      '<div class="field"><label>配置文件（可多选；未选的从当前版本继承）</label><input type="file" id="up-config" multiple></div>' +
      '<div class="muted">支持一次选中多个配置文件进行操作，切换版本时整套配置一起切换。</div>' +
      '<div id="up-progress" style="display:none">' +
      '<div class="progress-bar"><div class="progress-fill" id="up-fill"></div></div>' +
      '<div class="progress-text"><span id="up-pct"></span><span id="up-bytes"></span></div>' +
      '</div>' +
      '<div class="upload-error" id="up-error" style="display:none"></div>' +
      '<div class="version-actions" style="margin-top:16px">' +
      '<button class="secondary" data-cancel>取消</button>' +
      '<button id="up-submit">上传</button>' +
      '</div>';
    document.body.appendChild(dialog);
    dialog.showModal();

    let xhr = null;
    const close = () => { if (xhr) xhr.abort(); dialog.close(); };
    dialog.querySelector('[data-close]').addEventListener('click', close);
    dialog.querySelector('[data-cancel]').addEventListener('click', close);

    // 文件大小显示
    const binaryInput = dialog.querySelector('#up-binary');
    binaryInput.addEventListener('change', () => {
      const f = binaryInput.files[0];
      const size = f ? (f.size / 1024 / 1024).toFixed(1) + ' MB' : '';
      binaryInput.title = size;
    });

    dialog.querySelector('#up-submit').addEventListener('click', () => {
      const versionName = '';   // 版本号由后端自动递增，前端不再传
      const binaryFile = binaryInput.files[0];
      const configFiles = Array.from(dialog.querySelector('#up-config').files || []);
      if (!binaryFile) { toast('请选择二进制文件'); return; }
      if (configFiles.some((f) => !f.name || /[\\/]/.test(f.name) || f.name === '.' || f.name === '..')) {
        toast('配置文件名不合法（不能包含路径分隔符）');
        return;
      }

      const submitBtn = dialog.querySelector('#up-submit');
      const progress = dialog.querySelector('#up-progress');
      const fill = dialog.querySelector('#up-fill');
      const pct = dialog.querySelector('#up-pct');
      const bytes = dialog.querySelector('#up-bytes');
      const errEl = dialog.querySelector('#up-error');

      submitBtn.disabled = true;
      submitBtn.textContent = '上传中…';
      progress.style.display = 'block';
      errEl.style.display = 'none';

      uploadVersion(a.id, versionName, binaryFile, configFiles, (evt) => {
        const p = Math.round((evt.loaded / evt.total) * 100);
        fill.style.width = p + '%';
        pct.textContent = p + '%';
        bytes.textContent = (evt.loaded / 1024 / 1024).toFixed(1) + ' / ' + (evt.total / 1024 / 1024).toFixed(1) + ' MB';
      }).then((resp) => {
        fill.style.width = '100%';
        let msg = resp.version + ' 已上传并切换，参数发现中…';
        if (resp.config_files && resp.config_files.length) {
          msg += '；配置 ' + resp.config_files.length + ' 个';
          if (resp.inherited_configs && resp.inherited_configs.length) {
            msg += '（' + resp.inherited_configs.length + ' 个继承自上一版）';
          }
        }
        if (resp.pruned_versions && resp.pruned_versions.length) {
          msg += '；已按上限清理 ' + resp.pruned_versions.length + ' 个最旧版本';
        }
        toast(msg);
        dialog.close();
        if (onDone) onDone();
        loadActions();
      }).catch((e) => {
        errEl.style.display = 'block';
        errEl.textContent = uploadErrorMessage(e);
        submitBtn.disabled = false;
        submitBtn.textContent = '上传';
      });
    });
  }

  function uploadVersion(actionId, versionName, binaryFile, configFiles, onProgress) {
    return new Promise((resolve, reject) => {
      const xhr = new XMLHttpRequest();
      const form = new FormData();
      if (versionName) form.append('version_name', versionName);
      form.append('binary', binaryFile);
      (configFiles || []).forEach((f) => form.append('config', f));

      xhr.upload.onprogress = (e) => {
        if (e.lengthComputable && onProgress) onProgress(e);
      };
      xhr.onload = () => {
        let body = {};
        try { body = JSON.parse(xhr.responseText || '{}'); } catch (_) { console.warn('upload: 响应体非 JSON'); }
        if (xhr.status === 201) resolve(body);
        else reject({ status: xhr.status, error: body.error || 'unknown' });
      };
      xhr.onerror = () => reject({ status: 0, error: 'network_error' });
      xhr.onabort = () => reject({ status: 0, error: 'aborted' });

      xhr.open('POST', '/api/actions/' + actionId + '/versions');
      xhr.send(form);
    });
  }

  function uploadErrorMessage(e) {
    const map = {
      upload_disabled: '该 Action 未开启上传',
      version_exists: '版本名已存在，请换一个',
      binary_too_large: '二进制超过大小上限',
      config_too_large: '配置文件超过大小上限',
      config_required: '该 Action 需要配置文件',
      binary_missing: '请选择二进制文件',
      upload_in_progress: '该 Action 已有上传进行中',
      insufficient_storage: '磁盘空间不足',
      invalid_version_name: '版本名只能包含字母、数字、点、下划线、连字符',
      csrf_failed: '无权限',
      rate_limited: '请求过于频繁',
    };
    if (e.error && map[e.error]) return map[e.error];
    if (e.status === 401) return '登录已过期，请重新访问带 token 的链接';
    if (e.status === 403) return '无权限';
    if (e.status === 429) return '请求过于频繁';
    if (e.error === 'network_error') return '网络错误，请重试';
    return '上传失败';
  }

  // ============ 新增脚本 ============
  // ============ 分组管理 ============
  async function openGroupManager() {
    let data;
    try {
      data = await fetchJSON('/api/actions');
    } catch (e) { toast('加载失败'); return; }
    const dialog = document.createElement('dialog');
    dialog.innerHTML =
      '<div class="dialog-header"><span class="dialog-title">分组管理</span>' +
      '<button class="close-btn" data-close>×</button></div>' +
      '<div style="display:flex;gap:8px;margin-bottom:12px;">' +
      '<input type="text" id="gm-new-name" placeholder="新分组名称" style="flex:1;">' +
      '<button id="gm-add">新增</button>' +
      '</div>' +
      '<div class="muted" style="font-size:12px;margin-bottom:8px;">用 ↑ ↓ 调整分组的先后顺序，然后保存。</div>' +
      '<div id="gm-list"></div>' +
      '<div class="version-actions"><button id="gm-save-order">保存分组顺序</button></div>';
    document.body.appendChild(dialog);
    dialog.showModal();
    dialog.querySelector('[data-close]').addEventListener('click', () => dialog.close());

    const list = dialog.querySelector('#gm-list');
    // 收集分组及其 action；order 保存分组的先后顺序
    const groups = {};
    const order = [];
    (data.groups || []).forEach((g) => {
      groups[g.name] = (g.actions || []).map((a) => ({ id: a.id, title: a.title }));
      order.push(g.name);
    });

    // 保存分组先后顺序到后端（写入 config.yaml 的 group_order）
    dialog.querySelector('#gm-save-order').addEventListener('click', async () => {
      try {
        await fetchJSON('/api/groups/order', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ order }),
        });
        toast('分组顺序已保存');
        dialog.close();
        loadActions();
      } catch (e) {
        toast('保存顺序失败：' + ((e.body && e.body.message) || '请重试'));
      }
    });

    const renderGroups = () => {
      list.innerHTML = '';
      if (order.length === 0) {
        list.innerHTML = '<div class="muted">暂无分组</div>';
        return;
      }
      order.forEach((gname) => {
        const isDefault = gname === '默认';
        const row = document.createElement('div');
        row.style.cssText = 'display:flex;align-items:center;gap:8px;padding:8px;border-bottom:1px solid var(--border);';
        const nameInput = document.createElement('input');
        nameInput.type = 'text';
        nameInput.value = gname;
        nameInput.style.cssText = 'flex:1;';
        if (isDefault) nameInput.disabled = true;
        const count = document.createElement('span');
        count.className = 'muted';
        count.style.cssText = 'font-size:12px;min-width:60px;';
        count.textContent = (groups[gname] || []).length + ' 个脚本';
        // 上移 / 下移：调整分组的先后顺序
        const upBtn = document.createElement('button');
        upBtn.className = 'secondary';
        upBtn.textContent = '↑';
        upBtn.title = '上移';
        upBtn.style.cssText = 'font-size:12px;padding:3px 8px;';
        upBtn.disabled = order.indexOf(gname) === 0;
        upBtn.addEventListener('click', () => {
          const i = order.indexOf(gname);
          if (i > 0) {
            const t = order[i - 1]; order[i - 1] = order[i]; order[i] = t;
            renderGroups();
          }
        });
        const downBtn = document.createElement('button');
        downBtn.className = 'secondary';
        downBtn.textContent = '↓';
        downBtn.title = '下移';
        downBtn.style.cssText = 'font-size:12px;padding:3px 8px;';
        downBtn.disabled = order.indexOf(gname) === order.length - 1;
        downBtn.addEventListener('click', () => {
          const i = order.indexOf(gname);
          if (i >= 0 && i < order.length - 1) {
            const t = order[i + 1]; order[i + 1] = order[i]; order[i] = t;
            renderGroups();
          }
        });

        // 重命名（默认分组不可重命名）
        const renameBtn = document.createElement('button');
        renameBtn.className = 'secondary';
        renameBtn.textContent = '重命名';
        renameBtn.style.cssText = 'font-size:12px;padding:3px 10px;';
        if (isDefault) {
          renameBtn.disabled = true;
          renameBtn.title = '默认分组不可重命名';
        }
        renameBtn.addEventListener('click', async () => {
          const newName = nameInput.value.trim();
          if (!newName || newName === gname) return;
          if (groups[newName]) { toast('分组名已存在'); return; }
          // 批量重命名该分组下所有 action：逐条收集失败，不再假成功。
          const failed = [];
          for (const a of (groups[gname] || [])) {
            try {
              const full = await fetchJSON('/api/actions/' + a.id);
              full.group = newName;
              await fetchJSON('/api/actions/' + a.id, {
                method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(full),
              });
            } catch (e) {
              failed.push(a.title || a.id);
              console.warn('分组重命名：单个动作更新失败', a.id, e);
            }
          }
          if (failed.length) {
            toast('分组重命名部分失败：' + failed.length + ' 个动作未更新（' + failed.slice(0, 3).join('、') + '）');
          } else {
            toast('分组已重命名');
          }
          dialog.close();
          loadActions();
        });
        // 删除分组（默认分组不可删除）
        const delBtn = document.createElement('button');
        delBtn.className = 'danger';
        delBtn.textContent = '删除';
        delBtn.style.cssText = 'font-size:12px;padding:3px 10px;';
        if (isDefault) {
          delBtn.disabled = true;
          delBtn.title = '默认分组不可删除';
        }
        delBtn.addEventListener('click', async () => {
          const actions = groups[gname] || [];
          const target = '默认';
          if (actions.length > 0) {
            if (!confirm('分组「' + gname + '」内有 ' + actions.length + ' 个脚本，删除分组会把这些脚本移到「' + target + '」分组，确认？')) return;
            const failed = [];
            for (const a of actions) {
              try {
                const full = await fetchJSON('/api/actions/' + a.id);
                full.group = target;
                await fetchJSON('/api/actions/' + a.id, {
                  method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(full),
                });
              } catch (e) {
                failed.push(a.title || a.id);
                console.warn('分组删除：单个动作移动失败', a.id, e);
              }
            }
            if (failed.length) {
              toast('分组删除部分失败：' + failed.length + ' 个动作未移动（' + failed.slice(0, 3).join('、') + '）');
            }
          } else {
            if (!confirm('删除空分组「' + gname + '」？')) return;
          }
          toast('分组已删除');
          dialog.close();
          loadActions();
        });
        row.appendChild(nameInput);
        row.appendChild(count);
        row.appendChild(upBtn);
        row.appendChild(downBtn);
        row.appendChild(renameBtn);
        row.appendChild(delBtn);
        list.appendChild(row);
      });
    };
    renderGroups();

    // 新增分组：真正的空分组无法独立持久化（分组由脚本的 group 字段推导），
    // 因此引导到「新增脚本」时创建；若已有脚本想归入新分组，也可在编辑脚本时改分组。
    dialog.querySelector('#gm-add').addEventListener('click', () => {
      const name = dialog.querySelector('#gm-new-name').value.trim();
      if (!name) { toast('请输入分组名称'); return; }
      if (groups[name]) { toast('分组已存在'); return; }
      toast('分组「' + name + '」会在添加脚本时创建，请到「新增脚本」中选择');
      dialog.close();
      openCreateActionDialog();
    });
  }

  // ============ 编辑 / 删除脚本 ============
  async function deleteAction(a) {
    // 删除脚本会连同磁盘上的 .sh 一起删（日志保留），这里把后果说清楚
    let extra = '';
    if (a.has_script) {
      extra += '\n磁盘上的 sh 脚本会一并删除' + (a.script_path ? '：\n' + a.script_path : '') + '\n执行日志保留。';
    }
    if (a.script_ref) {
      extra += '\n该动作引用的是服务器上已有脚本，只删除动作，不会删文件：\n' + a.script_ref;
    }
    if (a.active_version) extra += '\n其版本目录也会一并删除。';
    if (!confirm('确定删除脚本「' + a.title + '」？' + extra)) return;
    const q = a.active_version ? '?remove_dir=true' : '';
    try {
      const r = await fetchJSON('/api/actions/' + a.id + q, { method: 'DELETE' });
      toast(r && r.removed_script ? '已删除，脚本文件已清理' : '已删除');
      loadActions();
    } catch (e) {
      toast('删除失败');
    }
  }

  async function openEditActionDialog(actionId) {
    let action;
    try {
      action = await fetchJSON('/api/actions/' + actionId);
    } catch (e) {
      toast('加载脚本信息失败');
      return;
    }
    const sched = action.schedule || {};
    const scriptDir = action.script_directory || '/opt/olivetin/scripts';
    const dialog = document.createElement('dialog');
    dialog.innerHTML =
      '<div class="dialog-header"><span class="dialog-title">编辑脚本</span>' +
      '<button class="close-btn" data-close>×</button></div>' +
      '<div class="field"><label>标题</label><input type="text" id="ea-title" value="' + escapeHtml(action.title || '') + '"></div>' +
      (action.versioning
        ? '<div class="field"><label>命令（版本管理脚本由版本目录决定）</label>' +
          '<textarea id="ea-command" rows="3" readonly style="width:100%;font-family:ui-monospace,monospace;font-size:13px;padding:8px;border:1px solid var(--border);border-radius:6px;background:var(--card);color:var(--text);">' + escapeHtml(action.command || '') + '</textarea></div>'
        : '<div class="field"><label>脚本来源</label><select id="ea-source">' +
          '<option value="command">编辑命令内容（保存时自动生成 .sh）</option>' +
          '<option value="ref">引用路由器上已有脚本（不复制、不覆盖）</option>' +
          '</select></div>' +
          '<div id="ea-cmd-area">' +
          '<div class="field"><label>命令 · 保存后写入下方 sh 脚本</label>' +
          '<textarea id="ea-command" rows="6" style="width:100%;font-family:ui-monospace,monospace;font-size:13px;padding:8px;border:1px solid var(--border);border-radius:6px;background:var(--card);color:var(--text);">' + escapeHtml(action.command || '') + '</textarea></div>' +
          '<div class="field"><label>从本地 .sh 文件重新导入（可选，会覆盖上面的命令）</label>' +
          '<input type="file" id="ea-shfile" accept=".sh,.bash,text/x-shellscript,application/x-sh,text/plain">' +
          '<div id="ea-import-status" class="muted" style="font-size:12px;margin-top:4px;display:none;"></div></div>' +
          '<div class="field"><label>脚本文件名（留空默认 ' + escapeHtml(action.id + '.sh') + '）</label>' +
          '<input type="text" id="ea-script-name" value="' + escapeHtml(action.script_name || '') + '" placeholder="' + escapeHtml(action.id + '.sh') + '"></div>' +
          '<div class="field"><label>脚本完整保存路径（可自定义）</label>' +
          '<input type="text" id="ea-script-path" class="mono" value="' + escapeHtml(action.script_path || '') + '"></div>' +
          '<div class="muted" style="font-size:12px;margin-top:-6px;">命令写入该 .sh 文件后由 /bin/sh 执行，重定向、管道、变量展开均可用；脚本内可用 $0 或 $OLIVETIN_SCRIPT_PATH 取得自身路径。</div>' +
          '</div>' +
          '<div id="ea-ref-area" style="display:none">' +
          '<div class="field"><label>脚本路径（路由器上的绝对路径）</label>' +
          '<input type="text" id="ea-ref-path" class="mono" value="' + escapeHtml(action.script_ref || '') + '" placeholder="/opt/olivetin/led-on.sh"></div>' +
          '<div style="display:flex;gap:8px;margin:-6px 0 8px;">' +
          '<button class="secondary" id="ea-ref-browse" style="font-size:12px;padding:4px 10px;">浏览服务器脚本</button>' +
          '<button class="secondary" id="ea-ref-check" style="font-size:12px;padding:4px 10px;">检查文件</button>' +
          '<button class="secondary" id="ea-ref-load" style="font-size:12px;padding:4px 10px;">读取内容到编辑器</button>' +
          '</div>' +
          '<div id="ea-ref-info" class="muted" style="font-size:12px;margin-bottom:8px;"></div>' +
          '<div class="muted" style="font-size:12px;">面板只在该路径上执行 /bin/sh，不读取、不覆盖内容；删动作时也不会删这个文件。脚本内可用 $0 或 $OLIVETIN_SCRIPT_PATH 取得自身路径。</div>' +
          '</div>') +
      (action.versioning
        ? '<div class="field"><label>版本内配置文件名（逗号分隔；新增账号在此登记）</label>' +
          '<input type="text" id="ea-config-files" value="' + escapeHtml(
            ((action.versioning.config_files || []).length
              ? action.versioning.config_files
              : (action.versioning.config_name ? [action.versioning.config_name] : [])
            ).join(', ')
          ) + '" placeholder="例如：wb-signin.json, xiaoning.json, xiaoyu.json"></div>'
        : '') +
      '<div class="field"><label>分组</label><select id="ea-group" style="width:100%;"></select></div>' +
      '<div class="field" id="ea-custom-group-field" style="display:none;"><label>自定义分组名</label><input type="text" id="ea-custom-group" placeholder="输入新分组名"></div>' +
      '<div class="field check-field"><label><input type="checkbox" id="ea-versioning" ' +
      ((action.versioning && action.versioning.enabled) ? 'checked' : '') +
      '> 启用版本管理（多版本上传 / 切换 / 回退）</label></div>' +
      '<div id="ea-ver-fields" style="display:' +
      ((action.versioning && action.versioning.enabled) ? '' : 'none') + '">' +
      '<div class="field"><label>版本目录（base_dir）</label><input type="text" id="ea-ver-basedir" value="' +
      escapeHtml((action.versioning && action.versioning.base_dir) || '') + '" placeholder="留空则自动创建 {脚本目录}/{id}"></div>' +
      '<div class="field"><label>程序文件名（含扩展名；留空沿用原值）</label><input type="text" id="ea-ver-binary" value="' +
      escapeHtml((action.versioning && action.versioning.binary_name) || '') + '" placeholder="run.sh"></div>' +
      '</div>' +
      '<div class="field"><label>描述</label><input type="text" id="ea-desc" value="' + escapeHtml(action.description || '') + '"></div>' +
      '<div class="field"><label>超时（秒）</label><input type="number" id="ea-timeout" value="' + (action.timeout || 30) + '"></div>' +
      '<div class="field"><label>并发上限</label><input type="number" id="ea-concurrency" value="' + (action.concurrency || 1) + '"></div>' +
      '<div class="field"><label>推送通知</label><input type="checkbox" id="ea-push" ' + (action.push ? 'checked' : '') + ' style="width:auto;"></div>' +
      '<div class="field" id="ea-push-cond-field" style="' + (action.push ? '' : 'display:none;') + '"><label>推送条件</label><select id="ea-push-cond">' +
      '<option value="always"' + ((action.push_condition || 'always') === 'always' ? ' selected' : '') + '>总是推送</option>' +
      '<option value="on_failure"' + (action.push_condition === 'on_failure' ? ' selected' : '') + '>仅失败时推送</option>' +
      '<option value="on_success"' + (action.push_condition === 'on_success' ? ' selected' : '') + '>仅成功时推送</option>' +
      '<option value="on_marker"' + (action.push_condition === 'on_marker' ? ' selected' : '') + '>脚本标记 [PUSH] 时推送</option>' +
      '</select></div>' +
      '<div id="ea-push-extra" style="' + (action.push ? '' : 'display:none;') + '">' +
      // 精简：只留「推送条件」与「内容类型」。
      // 推送标题/正文/接收者 UIDs/主题 IDs 不在界面暴露，但配置字段仍然有效，
      // 保存时按 action 里的原值原样带回去（下面提交逻辑里继承），不会被清空。
      '<div class="field"><label>内容类型</label><select id="ea-push-ctype">' +
      '<option value=""' + (action.push_content_type ? '' : ' selected') + '>跟随全局设置</option>' +
      '<option value="1"' + (action.push_content_type === 1 ? ' selected' : '') + '>纯文本</option>' +
      '<option value="2"' + (action.push_content_type === 2 ? ' selected' : '') + '>HTML</option>' +
      '<option value="3"' + (action.push_content_type === 3 ? ' selected' : '') + '>Markdown</option>' +
      '</select></div>' +
      '</div>' +
      '<hr style="border:0;border-top:1px solid var(--border);margin:12px 0;">' +
      '<div class="field"><label>启用定时</label><input type="checkbox" id="ea-sched-enabled" ' + (sched.enabled ? 'checked' : '') + ' style="width:auto;"></div>' +
      '<div class="field"><label>定时模式</label><select id="ea-sched-mode">' +
      '<option value="daily"' + (sched.mode === 'daily' ? ' selected' : '') + '>每天</option>' +
      '<option value="weekly"' + (sched.mode === 'weekly' ? ' selected' : '') + '>每周</option>' +
      '<option value="monthly"' + (sched.mode === 'monthly' ? ' selected' : '') + '>每月</option>' +
      '<option value="cron"' + (sched.mode === 'cron' ? ' selected' : '') + '>自定义 cron</option>' +
      '</select></div>' +
      '<div class="field" id="ea-time-field"><label>时间（HH:MM）</label><input type="text" id="ea-time" value="' + escapeHtml(sched.time || '08:00') + '"></div>' +
      '<div class="field" id="ea-weekdays-field" style="display:none"><label>星期（1-7，逗号分隔，1=周一）</label><input type="text" id="ea-weekdays" value="' + escapeHtml((sched.weekdays || []).join(',')) + '"></div>' +
      '<div class="field" id="ea-days-field" style="display:none"><label>日期（1-31，逗号分隔）</label><input type="text" id="ea-days" value="' + escapeHtml((sched.days || []).join(',')) + '"></div>' +
      '<div class="field" id="ea-cron-field" style="display:none"><label>cron 表达式（5段：分 时 日 月 周）</label><input type="text" id="ea-cron" value="' + escapeHtml(sched.cron || '0 2 * * *') + '"></div>' +
      '<div class="field"><label>时区</label><input type="text" id="ea-tz" value="' + escapeHtml(sched.timezone || '+08:00') + '"></div>' +
      '<hr style="border:0;border-top:1px solid var(--border);margin:12px 0;">' +
      '<div class="card-title" style="margin-bottom:8px;display:flex;align-items:center;gap:8px;">' +
      '<span style="flex:1;">执行参数（子命令）</span>' +
      '<button class="secondary" id="ea-rescan-subs" style="font-size:12px;padding:2px 10px;">重新读取子命令</button>' +
      '</div>' +
      '<div id="ea-subs"></div>' +
      '<button class="secondary" id="ea-add-sub" style="font-size:12px;padding:4px 10px;margin-bottom:12px;">+ 添加子命令</button>' +
      '<div class="muted" id="ea-subs-hint" style="font-size:12px;margin-bottom:12px;"></div>' +
      '<div class="version-actions" style="margin-top:16px">' +
      '<button class="secondary" data-cancel>取消</button>' +
      '<button id="ea-submit">保存</button>' +
      '</div>';
    document.body.appendChild(dialog);
    dialog.showModal();

    // 改脚本文件名时刷新默认路径；用户手动改过完整路径后不再覆盖
    const nameInput = dialog.querySelector('#ea-script-name');
    const pathInput = dialog.querySelector('#ea-script-path');
    // 已经自定义过完整路径的 action：初始就视为「已手动指定」，
    // 否则改文件名会把自定义路径悄悄覆盖回默认目录。
    let pathDirty = !!(action.script_full_path && String(action.script_full_path).trim());
    const defaultPath = () => scriptFullPath(scriptDir, nameInput ? nameInput.value : '', action.id);
    if (nameInput && pathInput) {
      nameInput.addEventListener('input', () => {
        if (!pathDirty) pathInput.value = defaultPath();
      });
      pathInput.addEventListener('input', () => {
        pathDirty = pathInput.value.trim().length > 0;
        if (!pathDirty) pathInput.value = defaultPath();
      });
    }
    // ===== 脚本来源切换：command（面板托管）/ ref（引用服务器已有脚本）=====
    const srcSel = dialog.querySelector('#ea-source');
    const cmdArea = dialog.querySelector('#ea-cmd-area');
    const refArea = dialog.querySelector('#ea-ref-area');
    const refPathEl = dialog.querySelector('#ea-ref-path');
    const refInfoEl = dialog.querySelector('#ea-ref-info');
    if (srcSel) {
      // 旧式 exec 动作（如 /opt/olivetin/led-on.sh）默认落在「引用」模式，
      // 命令框为空时保存也不会把它的 exec 弄坏。
      srcSel.value = (action.script_mode === 'ref' || action.script_mode === 'exec') ? 'ref' : 'command';
      const syncSrc = () => {
        const isRef = srcSel.value === 'ref';
        cmdArea.style.display = isRef ? 'none' : '';
        refArea.style.display = isRef ? '' : 'none';
      };
      srcSel.addEventListener('change', syncSrc);
      syncSrc();

      const checkRef = async () => {
        const p = refPathEl.value.trim();
        if (!p) { refInfoEl.textContent = ''; return; }
        refInfoEl.textContent = '检查中…';
        try {
          const d = await inspectRemoteScript(p);
          if (!d.exists) {
            refInfoEl.innerHTML = '<span class="err">文件不存在</span>：执行会失败，请确认路径。';
            return;
          }
          refInfoEl.innerHTML = '存在 · ' + escapeHtml(humanSize(d.size)) +
            (d.executable ? ' · 可执行' : ' · 无执行位（走 /bin/sh，不影响执行）') +
            (d.shebang ? ' · 首行 ' + escapeHtml(d.shebang) : '');
        } catch (e) {
          refInfoEl.innerHTML = '<span class="err">' +
            escapeHtml((e.body && (e.body.error || e.body.message)) || '检查失败') + '</span>';
        }
      };
      dialog.querySelector('#ea-ref-check').addEventListener('click', checkRef);
      refPathEl.addEventListener('change', checkRef);
      // 一键「收编」：服务器脚本内容 → 命令框，切换为面板托管
      dialog.querySelector('#ea-ref-load').addEventListener('click', async () => {
        const p = refPathEl.value.trim();
        if (!p) { toast('请先填写脚本路径'); return; }
        try {
          const d = await inspectRemoteScript(p);
          if (!d.exists) { toast('文件不存在'); return; }
          dialog.querySelector('#ea-command').value = d.content || '';
          if (nameInput && !nameInput.value.trim()) {
            const base = (p.split('/').pop() || 'imported').replace(/\.(sh|bash)$/i, '');
            nameInput.value = (base || 'imported') + '.sh';
            if (pathInput) pathInput.value = defaultPath();
          }
          srcSel.value = 'command';
          syncSrc();
          toast('已读取内容，保存后由面板接管该脚本（原文件保留、不再被此动作调用）');
        } catch (e) {
          toast((e.body && (e.body.error || e.body.message)) || '读取失败');
        }
      });
      dialog.querySelector('#ea-ref-browse').addEventListener('click', () => {
        const cur = refPathEl.value.trim();
        const startDir = cur.includes('/') ? (cur.slice(0, cur.lastIndexOf('/')) || '/') : scriptDir;
        openServerScriptPicker(startDir, (p) => { refPathEl.value = p; checkRef(); });
      });
      if (refPathEl.value.trim()) checkRef();
    }

    // 从本地 .sh 重新导入：覆盖命令原文（文件名不动）
    const shFileInput = dialog.querySelector('#ea-shfile');
    if (shFileInput) {
      shFileInput.addEventListener('change', async (ev) => {
        const f = ev.target.files && ev.target.files[0];
        if (!f) return;
        try {
          const text = await f.text();
          dialog.querySelector('#ea-command').value = text
            .replace(/^\uFEFF/, '').replace(/\r\n/g, '\n').replace(/\r/g, '\n');
          const st = dialog.querySelector('#ea-import-status');
          if (st) {
            const lines = text.split('\n').filter((l) => l.length > 0).length;
            st.style.display = '';
            st.textContent = '已导入 ' + f.name + '（' + lines + ' 行），内容已填入上方命令框；保存后生效。';
          }
          toast('已导入 ' + f.name + '，保存后生效');
        } catch (e) {
          toast('读取文件失败');
        }
      });
    }

    // 填充分组 select（含当前组 + 已有组 + 自定义选项）
    const groupSel3 = dialog.querySelector('#ea-group');
    const gset = new Set();
    gset.add('默认');
    if (action.group) gset.add(action.group);
    try {
      const actions = await fetchJSON('/api/actions');
      (actions.groups || []).forEach((g) => gset.add(g.name));
    } catch (e) { console.warn('分组列表加载失败，仅显示已有分组', e); }
    gset.forEach((g) => {
      const o = document.createElement('option');
      o.value = g; o.textContent = g;
      groupSel3.appendChild(o);
    });
    // 若当前组不在已有组里（自定义过的），加进去
    groupSel3.value = action.group || '默认';
    const customOpt = document.createElement('option');
    customOpt.value = '__custom__';
    customOpt.textContent = '自定义分组…';
    groupSel3.appendChild(customOpt);
    groupSel3.addEventListener('change', () => {
      dialog.querySelector('#ea-custom-group-field').style.display = groupSel3.value === '__custom__' ? 'block' : 'none';
    });

    dialog.querySelector('[data-close]').addEventListener('click', () => dialog.close());
    dialog.querySelector('[data-cancel]').addEventListener('click', () => dialog.close());

    // 推送开关切换：显示/隐藏推送条件与自定义推送项
    dialog.querySelector('#ea-push').addEventListener('change', (e) => {
      const show = e.target.checked ? '' : 'none';
      dialog.querySelector('#ea-push-cond-field').style.display = show;
      dialog.querySelector('#ea-push-extra').style.display = show;
    });

    // ===== 执行参数（子命令）=====
    // 可以有多行；打开时先用手填的配置，没有就用 --help 解析出的 flag 参数预填，
    // 用户可以增删。旧的 args 多参数体系保留字段但不再写入（保存时固定为空数组）。
    const collectArgs = () => [];

    const verCb = dialog.querySelector('#ea-versioning');
    const verFields = dialog.querySelector('#ea-ver-fields');
    if (verCb && verFields) {
      verCb.addEventListener('change', () => {
        verFields.style.display = verCb.checked ? '' : 'none';
      });
    }

    const subsBox = dialog.querySelector('#ea-subs');
    const subsHint = dialog.querySelector('#ea-subs-hint');
    const addSubRow = (val) => {
      const row = document.createElement('div');
      row.className = 'ea-sub-row';
      const inp = document.createElement('input');
      inp.type = 'text';
      inp.className = 'ea-sub-input';
      inp.placeholder = '例如：--check';
      inp.value = val || '';
      const del = document.createElement('button');
      del.type = 'button';
      del.className = 'danger ea-sub-del';
      del.textContent = '×';
      del.title = '移除这一项';
      del.addEventListener('click', () => { row.remove(); });
      row.appendChild(inp);
      row.appendChild(del);
      subsBox.appendChild(row);
      return inp;
    };

    // null/undefined = 从未配置（用 --help 预填）；[] = 用户显式清空（不要再补回来）
    const rawCfg = action.subcommands_config;
    const everConfigured = Array.isArray(rawCfg);
    const configured = (rawCfg || []).filter((x) => String(x).trim());
    let seed = configured.slice();
    let seedFromHelp = false;
    if (!everConfigured) {
      // 从 --help 解析结果预填：只取 flag 类型，过滤 help 自身
      seed = (action.params || [])
        .filter((p) => p.style === 'flag')
        .map((p) => String(p.name || '').replace(/^-+/, ''))
        .filter((n) => n && n.toLowerCase() !== 'help' && n.toLowerCase() !== 'h' && n.toLowerCase() !== 'usage')
        .map((n) => '--' + n);
      seedFromHelp = seed.length > 0;
    }
    (seed.length ? seed : ['']).forEach(addSubRow);
    if (subsHint) {
      subsHint.textContent = seedFromHelp
        ? '已从脚本 --help 自动解析并预填，可增删；不会出现 --help/-h 自身。'
        : '下拉里只会出现这里填的子命令；留空则执行时不带参数。';
    }
    dialog.querySelector('#ea-add-sub').addEventListener('click', () => {
      const inp = addSubRow('');
      inp.focus();
    });

    // 重新读取：重跑一次 --help 解析，用结果覆盖当前输入框
    const rescanBtn = dialog.querySelector('#ea-rescan-subs');
    if (rescanBtn) {
      rescanBtn.addEventListener('click', async () => {
        if (!action.id) { toast('新建脚本请先保存，保存后即可读取'); return; }
        const oldText = rescanBtn.textContent;
        rescanBtn.disabled = true;
        rescanBtn.textContent = '读取中…';
        // 弹窗关闭即取消在途请求与后续轮询，不再写已移除的 DOM 或误弹 toast。
        const rescanAbort = new AbortController();
        const onDialogClose = () => rescanAbort.abort();
        dialog.addEventListener('close', onDialogClose, { once: true });
        try {
          await fetchJSON('/api/actions/' + action.id + '/discover', { method: 'POST', signal: rescanAbort.signal });
          let params = [];
          // discover 是异步任务，轮询几次等它落缓存
          for (let i = 0; i < 12; i++) {
            await new Promise((r) => setTimeout(r, 400));
            if (rescanAbort.signal.aborted) return;
            const fresh = await fetchJSON('/api/actions/' + action.id, { signal: rescanAbort.signal });
            params = fresh.params || [];
            if (params.length) break;
          }
          if (rescanAbort.signal.aborted) return;
          const flags = params
            .filter((p) => p.style === 'flag')
            .map((p) => '--' + String(p.name || '').replace(/^-+/, ''))
            .filter((n) => {
              const bare = n.replace(/^-+/, '').toLowerCase();
              return bare && bare !== 'help' && bare !== 'h' && bare !== 'usage';
            });
          subsBox.innerHTML = '';
          (flags.length ? flags : ['']).forEach(addSubRow);
          if (subsHint) {
            subsHint.textContent = flags.length
              ? ('已重新读取到 ' + flags.length + ' 个子命令；保存后生效。')
              : '没有从 --help 读到子命令（脚本可能不支持 --help）。';
          }
          toast(flags.length ? ('读取到 ' + flags.length + ' 个子命令') : '没有读到子命令');
        } catch (e) {
          if (!rescanAbort.signal.aborted) toast('读取失败');
        } finally {
          dialog.removeEventListener('close', onDialogClose);
          if (!rescanAbort.signal.aborted) {
            rescanBtn.disabled = false;
            rescanBtn.textContent = oldText;
          }
        }
      });
    }

    // 模式切换
    const modeSel = dialog.querySelector('#ea-sched-mode');
    const syncFields = () => {
      const m = modeSel.value;
      dialog.querySelector('#ea-time-field').style.display = (m === 'daily' || m === 'weekly' || m === 'monthly') ? 'block' : 'none';
      dialog.querySelector('#ea-weekdays-field').style.display = m === 'weekly' ? 'block' : 'none';
      dialog.querySelector('#ea-days-field').style.display = m === 'monthly' ? 'block' : 'none';
      dialog.querySelector('#ea-cron-field').style.display = m === 'cron' ? 'block' : 'none';
    };
    modeSel.addEventListener('change', syncFields);
    syncFields();

    dialog.querySelector('#ea-submit').addEventListener('click', async () => {
      const newSched = { enabled: dialog.querySelector('#ea-sched-enabled').checked, skip_if_running: true };
      newSched.mode = modeSel.value;
      newSched.timezone = dialog.querySelector('#ea-tz').value.trim() || '+08:00';
      if (newSched.mode === 'daily' || newSched.mode === 'weekly' || newSched.mode === 'monthly') {
        newSched.time = dialog.querySelector('#ea-time').value.trim() || '08:00';
      }
      if (newSched.mode === 'weekly') {
        newSched.weekdays = dialog.querySelector('#ea-weekdays').value.split(',').map((s) => parseInt(s.trim(), 10)).filter((n) => !isNaN(n) && n >= 1 && n <= 7);
      }
      if (newSched.mode === 'monthly') {
        newSched.days = dialog.querySelector('#ea-days').value.split(',').map((s) => parseInt(s.trim(), 10)).filter((n) => !isNaN(n));
      }
      if (newSched.mode === 'cron') {
        newSched.cron = dialog.querySelector('#ea-cron').value.trim();
      }
      const cmdText = dialog.querySelector('#ea-command').value;
      // 按「脚本来源」组装：command=面板托管 / ref=引用服务器已有脚本 / versioning=保持原 exec
      let exec = action.exec || [];
      let command = null;
      let scriptName = null;
      if (!action.versioning && srcSel) {
        if (srcSel.value === 'ref') {
          const refP = refPathEl.value.trim();
          if (!refP) { toast('请填写脚本路径，或点「浏览服务器脚本」'); return; }
          // 后端校验为唯一权威（后端 Q6/Q7）：范围问题只提示、不拦截，继续提交由后端裁决。
          if (!refP.startsWith('/')) toast('脚本路径不是绝对路径（以 / 开头），是否可保存由服务端决定');
          if (!/\.(sh|bash)$/i.test(refP)) toast('脚本不是 .sh / .bash 后缀，是否可保存由服务端决定');
          command = null;
          // 原 exec 已经指向这个脚本就原样保留，避免无意义地改写配置
          exec = (action.exec || []).includes(refP) ? action.exec : ['/bin/sh', refP];
        } else if (cmdText.trim()) {
          command = cmdText;
          exec = [];   // 后端会写成 /bin/sh + 脚本绝对路径
          scriptName = nameInput ? nameInput.value.trim() : '';
        } else if (action.command) {
          toast('命令不能为空：想改用服务器上的脚本请把来源切到「引用」');
          return;
        } else {
          // 原本就是直接 exec 的动作，命令框没内容 → 原样保留
          exec = action.exec || [];
        }
      }
      // 自定义完整保存路径：与默认值相同就留空（继续跟随「脚本目录」设置），
      // 只有确实改到别处才写死到配置里。
      let scriptFullPath = null;
      if (command && !action.versioning && pathInput) {
        const typed = pathInput.value.trim();
        if (typed && typed !== defaultPath()) {
          if (!typed.startsWith('/')) toast('脚本完整路径不是绝对路径（以 / 开头），是否可保存由服务端决定');
          if (typed.endsWith('/')) {
            // 只填目录：文件名沿用「脚本文件名」或 {id}.sh
            const nm = (nameInput && nameInput.value.trim()) || (action.id + '.sh');
            scriptFullPath = typed + (nm.endsWith('.sh') ? nm : nm + '.sh');
          } else {
            scriptFullPath = typed.endsWith('.sh') ? typed : typed + '.sh';
          }
        }
      }
      const updated = {
        id: action.id,
        title: dialog.querySelector('#ea-title').value.trim(),
        // enabled 界面没有开关，但必须原样带回：Action.enabled 有 serde 默认 true，
        // 缺字段会被重置成「启用」，把配置里手写的 enabled: false 洗掉。
        enabled: action.enabled !== false,
        group: (() => {
          const v = dialog.querySelector('#ea-group').value;
          if (v === '__custom__') return dialog.querySelector('#ea-custom-group').value.trim() || '默认';
          return v || '默认';
        })(),
        description: dialog.querySelector('#ea-desc').value.trim() || null,
        exec,
        command,
        script_name: scriptName,
        script_full_path: scriptFullPath,
        timeout: parseInt(dialog.querySelector('#ea-timeout').value, 10) || 30,
        concurrency: parseInt(dialog.querySelector('#ea-concurrency').value, 10) || 1,
        push: dialog.querySelector('#ea-push').checked,
        push_condition: dialog.querySelector('#ea-push-cond').value,
        // 精简后界面只留「推送条件」和「内容类型」：
        // 标题/正文/接收者/主题不在 UI 暴露，但字段仍然有效 —— 原样继承，
        // 避免一次「保存」把配置文件里手写的自定义推送内容清空。
        push_title: action.push_title || null,
        push_content: action.push_content || null,
        push_uids: (action.push_uids && action.push_uids.length) ? action.push_uids : null,
        push_topic_ids: (action.push_topic_ids && action.push_topic_ids.length) ? action.push_topic_ids : null,
        // 只有一个子命令：填了就写 [值]，留空写 []。
        // 不能提交 action.subcommands（那是含历史/合并结果，可能带过期项）
        subcommands: Array.from(dialog.querySelectorAll('.ea-sub-input'))
          .map((x) => x.value.trim())
          .filter((x) => x.length > 0),
        push_content_type: (() => {
          const v = dialog.querySelector('#ea-push-ctype').value;
          return v ? parseInt(v, 10) : null;
        })(),
        env: action.env || {},
        versioning: (() => {
          const cb = dialog.querySelector('#ea-versioning');
          if (!cb || !cb.checked) return null;
          const base = (dialog.querySelector('#ea-ver-basedir') || {}).value || '';
          const bin = (dialog.querySelector('#ea-ver-binary') || {}).value || '';
          const prev = action.versioning || {};
          // 目录留空 = 让后端自动建 {脚本目录}/{id}，并把当前脚本复制成 v0.1
          return {
            enabled: true,
            base_dir: base.trim(),
            binary_name: bin.trim() || prev.binary_name || '',
            config_name: prev.config_name || null,
            config_files: prev.config_files || [],
            upload: prev.upload || { enabled: true, max_binary_mb: 50, max_config_kb: 256 },
          };
        })(),
        schedule: newSched.enabled ? newSched : null,
        args: collectArgs(),
        discover: action.discover || null,
      };
      // 登记/调整该 action 的配置文件集合（后续新增账号就在这里加名字）
      if (updated.versioning && dialog.querySelector('#ea-config-files')) {
        const names = dialog.querySelector('#ea-config-files').value
          .split(',').map((s) => s.trim()).filter((s) => s.length > 0);
        updated.versioning.config_files = names;
        updated.versioning.config_name = names[0] || null;
      }
      try {
        const saved = await fetchJSON('/api/actions/' + action.id, {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(updated),
        });
        toast(saved && saved.script_path ? '已保存：' + saved.script_path : '已保存');
        dialog.close();
        loadActions();
      } catch (e) {
        toast((e.body && (e.body.message || e.body.error)) || '保存失败');
      }
    });
  }

  async function openCreateActionDialog() {
    const settings = await loadUiSettings();
    const scriptDir = settings.script_directory || '/opt/olivetin/scripts';
    // 从标题生成 action id（与后端 script_filename 默认规则一致）
    const genId = (title) => {
      let id = title.toLowerCase().replace(/[^a-z0-9_-]+/g, '-').replace(/^-+|-+$/g, '');
      if (!id) id = 'script-' + Date.now().toString(36);
      return id;
    };
    const dialog = document.createElement('dialog');
    dialog.innerHTML =
      '<div class="dialog-header"><span class="dialog-title">快速添加脚本</span>' +
      '<button class="close-btn" data-close>×</button></div>' +
      '<div class="field"><label>标题 *</label><input type="text" id="ca-title" placeholder="例如：Ping 网关"></div>' +
      '<div class="field"><label>脚本来源</label><select id="ca-source">' +
      '<option value="command">编辑命令内容（保存时自动生成 .sh）</option>' +
      '<option value="ref">引用路由器上已有脚本（不复制、不覆盖）</option>' +
      '</select></div>' +
      '<div id="ca-cmd-area">' +
      '<div class="field"><label>导入本地 .sh 文件（可选，指你电脑上的文件）</label>' +
      '<input type="file" id="ca-shfile" accept=".sh,.bash,text/x-shellscript,application/x-sh,text/plain">' +
      '<div class="muted" style="font-size:12px;margin-top:4px;">选中后会把文件内容填进下面的命令框，并自动套用其文件名；也可以直接粘贴代码。要引用<b>路由器上</b>的脚本请把上面的来源改成「引用」。</div></div>' +
      '<div class="field"><label>命令（导入 .sh 或直接粘贴，二者填其一）</label>' +
      '<textarea id="ca-command" rows="8" placeholder="例如：echo 1 > /sys/class/leds/green/brightness" style="width:100%;font-family:ui-monospace,monospace;font-size:13px;padding:8px;border:1px solid var(--border);border-radius:6px;background:var(--card);color:var(--text);"></textarea>' +
      '<div id="ca-import-status" class="muted" style="font-size:12px;margin-top:4px;display:none;"></div></div>' +
      '<div class="field"><label>脚本文件名（可选）</label><input type="text" id="ca-script-name" placeholder="留空默认 {标题ID}.sh"></div>' +
      '<div class="field"><label>脚本完整保存路径（可自定义）</label>' +
      '<input type="text" id="ca-script-path" class="mono" placeholder="留空按「脚本目录 + 文件名」生成"></div>' +
      '<div class="muted" style="font-size:12px;margin-top:-6px;">留空即用默认目录；填绝对路径可把脚本放到任意位置（自动补 .sh）。执行时脚本内可用 $0 或 $OLIVETIN_SCRIPT_PATH 取到自身路径。</div>' +
      '</div>' +
      '<div id="ca-ref-area" style="display:none">' +
      '<div class="field"><label>脚本路径 *（路由器上的绝对路径）</label>' +
      '<input type="text" id="ca-ref-path" class="mono" placeholder="/opt/olivetin/led-on.sh"></div>' +
      '<div style="display:flex;gap:8px;margin:-6px 0 8px;">' +
      '<button class="secondary" id="ca-ref-browse" style="font-size:12px;padding:4px 10px;">浏览服务器脚本</button>' +
      '<button class="secondary" id="ca-ref-check" style="font-size:12px;padding:4px 10px;">检查文件</button>' +
      '<button class="secondary" id="ca-ref-load" style="font-size:12px;padding:4px 10px;">读取内容到编辑器</button>' +
      '</div>' +
      '<div id="ca-ref-info" class="muted" style="font-size:12px;margin-bottom:8px;"></div>' +
      '<div class="muted" style="font-size:12px;">面板只在该路径上执行 /bin/sh，<b>不读取、不覆盖</b>文件内容；删除动作时也不会删这个文件。要改内容就直接改文件（改完立即生效）。</div>' +
      '</div>' +
      '<div class="field"><label>分组</label><select id="ca-group" style="width:100%;"></select></div>' +
      '<div class="field" id="ca-custom-group-field" style="display:none;"><label>自定义分组名</label><input type="text" id="ca-custom-group" placeholder="输入新分组名"></div>' +
      '<div class="field"><label>描述（可选）</label><input type="text" id="ca-desc" placeholder="可选"></div>' +
      '<details style="margin-top:8px;"><summary style="cursor:pointer;color:var(--text-muted);font-size:13px;">高级：上传二进制（版本管理，含配置文件）</summary>' +
      '<div class="field" style="margin-top:8px;"><label>base_dir（版本目录）</label><input type="text" id="ca-basedir" placeholder="/opt/olivetin/myscript"></div>' +
      '<div class="field"><label>程序文件名（含扩展名，如 run.sh；留空用上传文件名）</label><input type="text" id="ca-binary" placeholder="run.sh"></div>' +
      '<div class="field"><label>上传程序文件 *（可执行文件或带 #!/bin/sh 的脚本）</label><input type="file" id="ca-binfile"></div>' +
      '<div class="field"><label>配置文件（可多选）</label><input type="file" id="ca-configfile" multiple></div>' +
      '<div class="muted" style="font-size:12px;margin-top:-6px;">支持一次选中多个配置文件进行操作，切换版本时整套配置一起切换。</div>' +
      '</details>' +
      '<div class="version-actions" style="margin-top:16px">' +
      '<button class="secondary" data-cancel>取消</button>' +
      '<button id="ca-submit">添加</button>' +
      '</div>';
    document.body.appendChild(dialog);
    dialog.showModal();

    const close = () => dialog.close();
    dialog.querySelector('[data-close]').addEventListener('click', close);
    dialog.querySelector('[data-cancel]').addEventListener('click', close);

    // 标题或文件名变化时刷新默认路径预览；用户手动改过路径后不再覆盖
    const titleInput = dialog.querySelector('#ca-title');    const nameInput = dialog.querySelector('#ca-script-name');
    const pathInput = dialog.querySelector('#ca-script-path');
    let pathDirty = false;
    const defaultPath = () => scriptFullPath(scriptDir, nameInput.value, genId(titleInput.value.trim()));
    const syncScriptPath = () => {
      if (pathDirty) return;
      pathInput.value = defaultPath();
    };
    titleInput.addEventListener('input', syncScriptPath);
    nameInput.addEventListener('input', syncScriptPath);
    pathInput.addEventListener('input', () => {
      // 清空 = 回到自动生成，重新跟随标题/文件名
      pathDirty = pathInput.value.trim().length > 0;
      if (!pathDirty) syncScriptPath();
    });
    syncScriptPath();

    // 导入本地 .sh：读取文本填入命令框，文件名作为脚本名（清洗成合法字符）
    // 并同步标题，避免出现「脚本名和标题对不上」的情况。
    dialog.querySelector('#ca-shfile').addEventListener('change', async (ev) => {
      const f = ev.target.files && ev.target.files[0];
      if (!f) return;
      let text = '';
      try {
        text = await f.text();
      } catch (e) {
        toast('读取文件失败');
        return;
      }
      // 去掉 BOM 与 CRLF，Windows 上导出的 .sh 直接粘进 shell 会报 not found
      text = text.replace(/^\uFEFF/, '').replace(/\r\n/g, '\n').replace(/\r/g, '\n');
      dialog.querySelector('#ca-command').value = text;
      const base = f.name.replace(/\.(sh|bash)$/i, '');
      const safe = base.replace(/[^A-Za-z0-9._-]+/g, '-').replace(/^[-.]+|[-.]+$/g, '').slice(0, 48);
      const scriptBase = safe || 'imported';
      nameInput.value = scriptBase + '.sh';
      if (!titleInput.value.trim()) titleInput.value = scriptBase;
      syncScriptPath();
      // 明确告诉用户「内容已经在下面了」，避免看到空命令框又去手填
      const st = dialog.querySelector('#ca-import-status');
      if (st) {
        const lines = text.split('\n').filter((l) => l.length > 0).length;
        st.style.display = '';
        st.textContent = '已导入 ' + f.name + '（' + lines + ' 行），内容已填入上方命令框；保存后由面板生成脚本。';
      }
      toast('已导入 ' + f.name);
    });

    // ===== 脚本来源切换 =====
    // command：面板托管（命令写入 .sh，每次执行前重写）
    // ref    ：引用路由器上已有脚本（只调用，不读不写不删）
    const sourceSel = dialog.querySelector('#ca-source');
    const cmdArea = dialog.querySelector('#ca-cmd-area');
    const refArea = dialog.querySelector('#ca-ref-area');
    const refPathInput = dialog.querySelector('#ca-ref-path');
    const refInfo = dialog.querySelector('#ca-ref-info');
    const syncSource = () => {
      const isRef = sourceSel.value === 'ref';
      cmdArea.style.display = isRef ? 'none' : '';
      refArea.style.display = isRef ? '' : 'none';
    };
    sourceSel.addEventListener('change', syncSource);
    syncSource();

    const checkRef = async () => {
      const p = refPathInput.value.trim();
      if (!p) { refInfo.textContent = ''; return; }
      refInfo.textContent = '检查中…';
      try {
        const d = await inspectRemoteScript(p);
        if (!d.exists) {
          refInfo.innerHTML = '<span class="err">文件不存在</span>：保存后执行会失败，请确认路径。';
          return;
        }
        refInfo.innerHTML = '存在 · ' + escapeHtml(humanSize(d.size)) +
          (d.executable ? ' · 可执行' : ' · 无执行位（走 /bin/sh，不影响执行）') +
          (d.shebang ? ' · 首行 ' + escapeHtml(d.shebang) : '') +
          (d.truncated ? ' · 文件较大，仅预览前 512KB' : '');
      } catch (e) {
        refInfo.innerHTML = '<span class="err">' +
          escapeHtml((e.body && (e.body.error || e.body.message)) || '检查失败') + '</span>';
      }
    };
    dialog.querySelector('#ca-ref-check').addEventListener('click', checkRef);
    refPathInput.addEventListener('change', checkRef);
    // 一键「收编」：把服务器脚本内容读进命令框并切到托管模式
    dialog.querySelector('#ca-ref-load').addEventListener('click', async () => {
      const p = refPathInput.value.trim();
      if (!p) { toast('请先填写脚本路径'); return; }
      try {
        const d = await inspectRemoteScript(p);
        if (!d.exists) { toast('文件不存在'); return; }
        dialog.querySelector('#ca-command').value = d.content || '';
        const base = (p.split('/').pop() || 'imported').replace(/\.(sh|bash)$/i, '');
        if (!nameInput.value.trim()) nameInput.value = (base || 'imported') + '.sh';
        sourceSel.value = 'command';
        syncSource();
        syncScriptPath();
        toast('已读取内容，改为面板托管（保存后写入默认目录）');
      } catch (e) {
        toast((e.body && (e.body.error || e.body.message)) || '读取失败');
      }
    });
    dialog.querySelector('#ca-ref-browse').addEventListener('click', () => {
      const cur = refPathInput.value.trim();
      const startDir = cur.includes('/')
        ? (cur.slice(0, cur.lastIndexOf('/')) || '/')
        : scriptDir;
      openServerScriptPicker(startDir, (p) => { refPathInput.value = p; checkRef(); });
    });

    // 填充已有分组到 select（含「默认」+ 自定义选项）
    const groupSel = dialog.querySelector('#ca-group');
    const gset = new Set();
    gset.add('默认');
    try {
      const actions = await fetchJSON('/api/actions');
      (actions.groups || []).forEach((g) => gset.add(g.name));
    } catch (e) { console.warn('分组列表加载失败，仅显示已有分组', e); }
    gset.forEach((g) => {
      const o = document.createElement('option');
      o.value = g; o.textContent = g;
      groupSel.appendChild(o);
    });
    const customOpt = document.createElement('option');
    customOpt.value = '__custom__';
    customOpt.textContent = '自定义分组…';
    groupSel.appendChild(customOpt);
    // 切换自定义输入框
    groupSel.addEventListener('change', () => {
      const isCustom = groupSel.value === '__custom__';
      dialog.querySelector('#ca-custom-group-field').style.display = isCustom ? 'block' : 'none';
    });

    dialog.querySelector('#ca-submit').addEventListener('click', async () => {
      const title = dialog.querySelector('#ca-title').value.trim();
      const command = dialog.querySelector('#ca-command').value.trim();
      const groupSel2 = dialog.querySelector('#ca-group');
      let group = groupSel2.value;
      if (group === '__custom__') {
        group = dialog.querySelector('#ca-custom-group').value.trim();
      }
      if (!group) group = '默认';
      const desc = dialog.querySelector('#ca-desc').value.trim();
      const baseDir = dialog.querySelector('#ca-basedir').value.trim();
      const binFile = dialog.querySelector('#ca-binfile').files[0];
      // 程序名留空时直接用上传的文件名（.sh 也能作为版本管理的主体）
      const binaryName = dialog.querySelector('#ca-binary').value.trim()
        || (binFile ? binFile.name : '');
      // 多配置文件：文件名即版本目录里的配置文件名
      const configFiles = Array.from(dialog.querySelector('#ca-configfile').files || []);
      const configNames = configFiles.map((f) => f.name);

      // 高级模式：上传二进制（版本管理，二进制 + 整套配置作为一个版本）
      if (baseDir && binaryName && binFile) {
        if (!title) { toast('请填写标题'); return; }
        const id = genId(title);
        const action = {
          id, title, group, description: desc || null,
          exec: [baseDir.replace(/\/$/, '') + '/current/' + binaryName],
          timeout: 30, concurrency: 1, push: false,
          versioning: {
            enabled: true, base_dir: baseDir, binary_name: binaryName,
            config_name: configNames[0] || null,
            config_files: configNames,
            upload: { enabled: true, max_binary_mb: 50, max_config_kb: 256 },
          },
        };
        const form = new FormData();
        form.append('action', JSON.stringify(action));
        form.append('binary', binFile);
        configFiles.forEach((f) => form.append('config', f));
        try {
          const resp = await fetch('/api/actions', { method: 'POST', body: form });
          if (resp.status === 401) { toast('登录已过期，请重新登录'); return; }
          if (resp.status === 429) { toast('请求过于频繁，请稍后再试'); return; }
          if (!resp.ok) { let b = {}; try { b = await resp.json(); } catch (_) { console.warn('创建脚本: 错误响应体非 JSON'); } toast(b.message || b.error || '创建失败'); return; }
          toast('脚本已创建'); dialog.close(); loadActions();
        } catch (e) { toast('创建失败'); }
        return;
      }

      // 引用模式：脚本已经在路由器上，面板只负责调用（不复制、不覆盖、删除时不删）
      if (sourceSel.value === 'ref') {
        if (!title) { toast('请填写标题'); return; }
        const refP = refPathInput.value.trim();
        if (!refP) { toast('请填写脚本路径，或点「浏览服务器脚本」选择'); return; }
        if (!refP.startsWith('/')) toast('脚本路径不是绝对路径（以 / 开头），是否可保存由服务端决定');
        if (!/\.(sh|bash)$/i.test(refP)) toast('脚本不是 .sh / .bash 后缀，是否可保存由服务端决定');
        const id = genId(title);
        const action = {
          id, title, group, description: desc || null,
          exec: ['/bin/sh', refP], command: null,
          timeout: 30, concurrency: 1, push: false,
        };
        try {
          const form = new FormData();
          form.append('action', JSON.stringify(action));
          const resp = await fetch('/api/actions', { method: 'POST', body: form });
          if (resp.status === 401) { toast('登录已过期，请重新登录'); return; }
          if (resp.status === 429) { toast('请求过于频繁，请稍后再试'); return; }
          if (!resp.ok) { let b = {}; try { b = await resp.json(); } catch (_) { console.warn('创建脚本: 错误响应体非 JSON'); } toast(b.message || b.error || '创建失败'); return; }
          toast('已添加，引用 ' + refP);
          dialog.close(); loadActions();
        } catch (e) { toast('创建失败'); }
        return;
      }

      // 快速模式：命令原文写入 sh 脚本，由后端生成脚本文件与 exec
      if (!title) { toast('请填写标题'); return; }
      if (!command) {
        toast('请填写命令，或点上方的「浏览…」导入 .sh 文件');
        return;
      }
      const typedPath = pathInput.value.trim();
      if (typedPath && !typedPath.startsWith('/')) {
        toast('脚本完整路径不是绝对路径（以 / 开头），是否可保存由服务端决定');
      }
      const id = genId(title);
      // 只有用户确实自定义了路径才写死；与默认值相同则留空，跟随「脚本目录」设置
      const customPath = (!typedPath || typedPath === defaultPath())
        ? null
        : (() => {
            // 只填目录（以 / 结尾）时，文件名沿用「脚本文件名」或 {id}.sh，
            // 否则会拼成 `/opt/.sh`
            if (typedPath.endsWith('/')) {
              const nm = (dialog.querySelector('#ca-script-name').value.trim() || (id + '.sh'));
              return typedPath + (nm.endsWith('.sh') ? nm : nm + '.sh');
            }
            return typedPath.endsWith('.sh') ? typedPath : typedPath + '.sh';
          })();
      const action = {
        id, title, group, description: desc || null,
        exec: [], command,
        script_name: dialog.querySelector('#ca-script-name').value.trim() || null,
        script_full_path: customPath,
        timeout: 30, concurrency: 1, push: false,
      };
      try {
        const form = new FormData();
        form.append('action', JSON.stringify(action));
        const resp = await fetch('/api/actions', { method: 'POST', body: form });
        if (resp.status === 401) { toast('登录已过期，请重新登录'); return; }
        if (resp.status === 429) { toast('请求过于频繁，请稍后再试'); return; }
        if (!resp.ok) { let b = {}; try { b = await resp.json(); } catch (_) { console.warn('创建脚本: 错误响应体非 JSON'); } toast(b.message || b.error || '创建失败'); return; }
        // 回显真实落盘路径，确认脚本文件到底写到了哪里
        let saved = {};
        try { saved = await resp.json(); } catch (_) { console.warn('创建脚本: 成功响应体非 JSON'); }
        toast(saved.script_path ? '脚本已添加：' + saved.script_path : '脚本已添加');
        dialog.close(); loadActions();
      } catch (e) { toast('创建失败'); }
    });
  }

  // ============ 脚本详情页 ============
  async function loadActionDetail(actionId) {
    const root = document.getElementById('action-root');
    if (!root) return;
    root.innerHTML = '<div class="muted">加载中…</div>';
    let action;
    try {
      action = await fetchJSON('/api/actions/' + actionId);
    } catch (e) {
      root.innerHTML = '<div class="muted">脚本不存在或加载失败</div>';
      return;
    }

    root.innerHTML = '';
    // 返回按钮
    const back = document.createElement('a');
    back.href = '/';
    back.textContent = '← 返回';
    back.style.cssText = 'display:inline-block;margin-bottom:12px;color:var(--accent);text-decoration:none;font-size:14px;';
    root.appendChild(back);

    // 标题栏
    const header = document.createElement('div');
    header.style.cssText = 'display:flex;justify-content:space-between;align-items:center;flex-wrap:wrap;gap:8px;margin-bottom:12px;';
    const h1 = document.createElement('h1');
    h1.textContent = action.title;
    h1.style.margin = '0';
    header.appendChild(h1);
    const btns = document.createElement('div');
    btns.style.cssText = 'display:flex;gap:8px;';
    const runBtn = document.createElement('button');
    runBtn.textContent = '执行';
    const stopBtn = document.createElement('button');
    stopBtn.className = 'danger';
    stopBtn.textContent = '停止';
    stopBtn.style.display = 'none';
    const editBtn = document.createElement('button');
    editBtn.className = 'secondary';
    editBtn.textContent = '编辑';
    editBtn.addEventListener('click', () => openEditActionDialog(actionId));
    // 子命令下拉：紧贴执行按钮右侧（VSCode 风格的分裂按钮），默认不带参数
    let selectedSub = null;
    if (Array.isArray(action.subcommands) && action.subcommands.length) {
      const wrap = document.createElement('div');
      wrap.className = 'split-wrap';
      const caret = document.createElement('button');
      caret.type = 'button';
      caret.className = 'split-caret';
      caret.id = 'ad-subtoggle';
      caret.title = '选择子命令（不选则默认不带参数）';
      caret.textContent = '▾';
      const menu = document.createElement('div');
      menu.className = 'split-menu';
      menu.id = 'ad-submenu';
      menu.style.display = 'none';
      const items = [{ v: '', t: '默认（不带参数）' }].concat(
        action.subcommands.map((sc) => ({ v: sc, t: sc }))
      );
      items.forEach((it) => {
        const b = document.createElement('button');
        b.type = 'button';
        b.className = 'split-item' + (it.v === '' ? ' active' : '');
        b.textContent = it.t;
        b.addEventListener('click', () => {
          selectedSub = it.v || null;
          menu.querySelectorAll('.split-item').forEach((x) => x.classList.remove('active'));
          b.classList.add('active');
          menu.style.display = 'none';
          caret.textContent = it.v ? '▾' : '▾';
          caret.title = it.v ? ('当前子命令：' + it.v) : '选择子命令（不选则默认不带参数）';
        });
        menu.appendChild(b);
      });
      // 执行按钮与箭头同属一个容器，中间无间隙；与其它按钮仍保持 8px
      wrap.appendChild(runBtn);
      wrap.appendChild(caret);
      wrap.appendChild(menu);
      btns.appendChild(wrap);
      caret.addEventListener('click', (ev) => {
        ev.stopPropagation();
        menu.style.display = menu.style.display === 'none' ? 'block' : 'none';
      });
      if (detailClickAbort) detailClickAbort.abort();
      detailClickAbort = new AbortController();
      document.addEventListener('click', (ev) => {
        // EventTarget 在 DOM 点击场景下就是 Node，用类型断言而非 any。
        if (!wrap.contains(/** @type {Node} */ (ev.target))) menu.style.display = 'none';
      }, { signal: detailClickAbort.signal });
    }
    if (!btns.contains(runBtn)) btns.appendChild(runBtn);
    btns.appendChild(stopBtn);
    btns.appendChild(editBtn);
    header.appendChild(btns);
    root.appendChild(header);

    // 元信息
    const meta = document.createElement('div');
    meta.className = 'card';
    const metaHtml = [];
    if (action.group) metaHtml.push('<span class="tag">分组：' + escapeHtml(action.group) + '</span>');
    if (action.active_version) metaHtml.push('<span class="badge">版本：' + escapeHtml(action.active_version) + '</span>');
    const schedDisp = action.schedule_display;
    if (schedDisp && schedDisp.enabled) {
      metaHtml.push('<span class="tag">🕐 ' + escapeHtml(schedDisp.description || '') + '</span>');
      if (schedDisp.next_run_human) metaHtml.push('<div class="meta-line">下次触发：' + escapeHtml(schedDisp.next_run_human) + '</div>');
    }
    if (action.description) metaHtml.push('<div class="card-desc">' + escapeHtml(action.description) + '</div>');
    // 脚本位置：面板托管的显示托管路径，引用型显示被引用的服务器文件
    const shownScript = action.command ? action.script_path : (action.script_ref || null);
    if (shownScript) {
      metaHtml.push('<div class="meta-line mono" style="word-break:break-all;">脚本路径：' + escapeHtml(shownScript) +
        (action.command ? '' : '（引用，不接管内容）') + '</div>');
    }
    meta.innerHTML = metaHtml.join(' ');
    root.appendChild(meta);

    // 执行参数说明：始终展示脚本支持的参数（来自 --help 解析），help 自身不列
    const paramInfo = document.createElement('div');
    paramInfo.className = 'card';
    const infoList = (action.params || []).filter((p) => {
      const n = String(p.name || '').toLowerCase().replace(/^-+/, '');
      return n !== 'help' && n !== 'h' && n !== 'usage';
    });
    const infoHtml = infoList.map((p) => {
      const hint = p.type === 'bool' ? '开关' : (p.required ? '必填' : '');
      return '<div class="help-param-row"><code>--' + escapeHtml(String(p.name).replace(/^-+/, '')) +
        '</code><span>' + escapeHtml(p.description || '') +
        (hint ? '　<em class="param-kind">' + hint + '</em>' : '') + '</span></div>';
    }).join('');
    paramInfo.innerHTML = '<div class="card-title">执行参数</div>' +
      (infoHtml
        ? '<div class="help-param-list">' + infoHtml + '</div>'
        : '<div class="meta-line">该脚本没有提供 --help 说明；执行时可用右上角下拉选择子命令。</div>');
    root.appendChild(paramInfo);

    // 参数表单（只用「手写参数」渲染；params 里含 --help 自动发现的项，不铺成表单）
    if (action.args && action.args.length > 0) {
      const form = document.createElement('div');
      form.className = 'card';
      const ft = document.createElement('div');
      ft.className = 'card-title';
      ft.textContent = '执行参数';
      form.appendChild(ft);
      const argsInputs = {};
      action.args.forEach((p) => {
        const field = document.createElement('div');
        field.className = 'field';
        const label = document.createElement('label');
        label.textContent = p.description || p.name;
        if (p.required) label.classList.add('required');
        field.appendChild(label);
        let input;
        if (p.options && p.options.length) {
          input = document.createElement('select');
          p.options.forEach((o) => { const op = document.createElement('option'); op.value = o; op.textContent = o; input.appendChild(op); });
        } else if (p.type === 'bool' || p.style === 'flag') {
          input = document.createElement('input');
          input.type = 'checkbox';
          input.style.width = 'auto';
        } else {
          input = document.createElement('input');
          input.type = p.type === 'int' ? 'number' : 'text';
          if (p.default) input.value = p.default;
        }
        argsInputs[p.name] = input;
        field.appendChild(input);
        form.appendChild(field);
      });
      root.appendChild(form);

      runBtn.addEventListener('click', async () => {
        const args = {};
        Object.keys(argsInputs).forEach((k) => {
          const el = argsInputs[k];
          args[k] = el.type === 'checkbox' ? (el.checked ? 'true' : 'false') : el.value;
        });
        doRun(root, actionId, args, runBtn, stopBtn, refreshLogs, {
          subcommand: selectedSub,
        });
      });
    } else {
      runBtn.addEventListener('click', async () => {
        doRun(root, actionId, {}, runBtn, stopBtn, refreshLogs, {
          subcommand: selectedSub,
        });
      });
    }

    stopBtn.addEventListener('click', async () => {
      try {
        await fetchJSON('/api/actions/' + actionId + '/stop', { method: 'POST' });
        toast('已停止');
        refreshLogs();
      } catch (e) {
        toast('停止失败');
      }
    });

    // 版本管理按钮
    if (action.active_version) {
      const verBtn = document.createElement('button');
      verBtn.className = 'secondary';
      verBtn.textContent = '版本管理';
      verBtn.addEventListener('click', () => openVersionPanel({ id: actionId, title: action.title, upload_enabled: action.upload_enabled }));
      btns.appendChild(verBtn);
    }

    // 日志
    const logsTitle = document.createElement('h2');
    logsTitle.textContent = '执行日志';
    logsTitle.style.cssText = 'font-size:16px;margin:20px 0 8px;';
    root.appendChild(logsTitle);
    const logsWrap = document.createElement('div');
    logsWrap.style.cssText = 'overflow-x:auto;';
    root.appendChild(logsWrap);
    const logsPager = document.createElement('div');
    logsPager.className = 'logs-pager';
    logsPager.style.cssText = 'display:flex;justify-content:center;gap:8px;margin:12px 0;';
    root.appendChild(logsPager);

    // 分页契约：GET /api/logs?action_id=<id>&limit=30[&before_id=<cursor>]
    // 响应：items(按 id 降序) / has_more / next_before_id。
    // 首次拉第一页；点「加载更多」把 next_before_id 作为下一页 before_id 追加；
    // 执行期 1s 轮询只比对第一页签名，有变化才重置回第一页重渲染。
    const LOG_PAGE_SIZE = 30;
    let logItems = [];
    let logNextBeforeId = null;
    let logHasMore = false;
    let lastLogSignature = '';

    const renderPager = () => {
      logsPager.replaceChildren();
      if (!logHasMore || !logNextBeforeId) return;
      const more = document.createElement('button');
      more.className = 'secondary';
      more.textContent = '加载更多（每页 ' + LOG_PAGE_SIZE + ' 条）';
      more.addEventListener('click', async () => {
        if (!logNextBeforeId) return;
        more.disabled = true;
        more.textContent = '加载中…';
        try {
          /** @type {LogsResponse} */
          const d = await fetchJSON('/api/logs?limit=' + LOG_PAGE_SIZE
            + '&before_id=' + logNextBeforeId
            + '&action_id=' + encodeURIComponent(actionId));
          const hidden = getHiddenLogs(actionId);
          const extra = (d.items || []).filter((it) => !hidden.has(it.id));
          // 条数上限：避免反复「加载更多」后数组与每次全量重建的 DOM 无限增长。
          const room = Math.max(0, 500 - logItems.length);
          logItems = logItems.concat(extra.slice(0, room));
          if (extra.length > room) toast('已达 500 条显示上限，请缩小筛选条件');
          logNextBeforeId = d.next_before_id || null;
          logHasMore = !!d.has_more && !!logNextBeforeId;
          renderLogTable(logsWrap, logItems, actionId, refreshLogs);
          renderPager();
        } catch (e) {
          toast('加载更多失败');
          more.disabled = false;
          more.textContent = '加载更多（每页 ' + LOG_PAGE_SIZE + ' 条）';
        }
      });
      logsPager.appendChild(more);
    };

    const refreshLogs = async () => {
      try {
        /** @type {LogsResponse} */
        const d = await fetchJSON('/api/logs?limit=' + LOG_PAGE_SIZE
          + '&action_id=' + encodeURIComponent(actionId));
        // 详情页「删除」只隐藏本页展示，不影响日志总页：用 localStorage 记录已隐藏的 id
        const hidden = getHiddenLogs(actionId);
        const items = (d.items || []).filter((it) => !hidden.has(it.id));
        // 1s 轮询期间数据没变就不重建表格，保住展开行 / 滚动位置 / 焦点。
        const signature = items.map((it) => it.id).join(',') + '|' + hidden.size;
        if (signature === lastLogSignature) return;
        lastLogSignature = signature;
        logItems = items;
        logNextBeforeId = d.next_before_id || null;
        logHasMore = !!d.has_more && !!logNextBeforeId;
        renderLogTable(logsWrap, logItems, actionId, refreshLogs);
        renderPager();
      } catch (e) {
        toast('日志加载失败');
      }
    };
    await refreshLogs();
  }

  // 获取详情页已隐藏（删除）的日志 id 集合
  function getHiddenLogs(actionId) {
    let map = {};
    try { map = JSON.parse(localStorage.getItem('olivetin-hidden-logs') || '{}'); } catch (e) { console.warn('隐藏日志记录解析失败', e); }
    return new Set(map[actionId] || []);
  }
  // 记录一条日志为隐藏（详情页删除，不影响总页）
  function hideLog(actionId, logId) {
    let map = {};
    try { map = JSON.parse(localStorage.getItem('olivetin-hidden-logs') || '{}'); } catch (e) { console.warn('隐藏日志记录解析失败', e); }
    if (!map[actionId]) map[actionId] = [];
    if (!map[actionId].includes(logId)) map[actionId].push(logId);
    localStorage.setItem('olivetin-hidden-logs', JSON.stringify(map));
  }

  function renderLogTable(wrap, items, actionId, onDelete) {
    wrap.innerHTML = '';
    if (!items || items.length === 0) {
      wrap.innerHTML = '<div class="muted">暂无执行记录</div>';
      return;
    }
    const table = document.createElement('table');
    table.className = 'logs';
    table.innerHTML = '<thead><tr><th>时间</th><th>触发</th><th>版本</th><th>退出码</th><th>耗时</th><th>推送</th><th>操作</th></tr></thead>';
    const tbody = document.createElement('tbody');
    items.forEach((e) => {
      const tr = document.createElement('tr');
      const trigger = e.trigger === 'manual' ? '手动' : '定时';
      const push = e.push_status === 'ok' ? '✓' : (e.push_status && e.push_status.failed ? '⚠' : '—');
      const status = e.exit_code === 0 ? '✓ 0' : '✗ ' + (e.exit_code === null ? '-' : escapeHtml(e.exit_code));
      tr.innerHTML =
        '<td>' + escapeHtml(e.started_at_local || e.started_at) + '</td>' +
        '<td>' + trigger + '</td>' +
        '<td>' + escapeHtml(e.action_version || '—') + '</td>' +
        '<td>' + status + '</td>' +
        '<td>' + (e.duration_ms / 1000).toFixed(1) + 's</td>' +
        '<td>' + push + '</td>' +
        '<td></td>';
      tr.style.cursor = 'pointer';
      // 展开详情：点击整行（除操作列）
      tr.addEventListener('click', (ev) => {
        if (ev.target.closest('button')) return;
        const detail = document.createElement('tr');
        detail.innerHTML = '<td colspan="7"><pre style="white-space:pre-wrap;word-break:break-all">' +
          escapeHtml((e.stdout || '') + (e.stderr ? '\n--- stderr ---\n' + e.stderr : '')) + '</pre></td>';
        if (tr.nextSibling && tr.nextSibling.dataset.detail) tr.nextSibling.remove();
        else { detail.dataset.detail = '1'; tr.after(detail); }
      });
      // 删除按钮
      const delBtn = document.createElement('button');
      delBtn.className = 'danger';
      delBtn.textContent = '删除';
      delBtn.style.cssText = 'font-size:12px;padding:2px 8px;';
      delBtn.addEventListener('click', async () => {
        if (!confirm('删除这条日志？')) return;
        // 详情页删除 = 仅隐藏本页展示（localStorage 记录），日志总页保留完整记录
        hideLog(actionId, e.id);
        toast('已在本机隐藏（其它浏览器仍可见）');
        if (onDelete) onDelete();
      });
      tr.querySelector('td:last-child').appendChild(delBtn);
      tbody.appendChild(tr);
    });
    table.appendChild(tbody);
    wrap.appendChild(table);
  }

  // 异步执行：发起后立即返回，轮询日志直到结束；执行期间显示停止按钮
  async function doRun(root, actionId, args, runBtn, stopBtn, refreshLogs, opts) {
    const extra = opts || {};
    const subLabel = extra.subcommand ? String(extra.subcommand).replace(/^--?/, '') : '';
    runBtn.disabled = true;
    runBtn.textContent = subLabel ? ('执行 ' + subLabel + ' 命令中…') : '执行中…';
    stopBtn.style.display = 'inline-block';
    try {
      const body = { args };
      // 只有真的选了子命令 / 填了附加参数才带上，保持请求体干净
      if (extra.subcommand) body.subcommand = extra.subcommand;
      await fetchJSON('/api/actions/' + actionId + '/run', {
        method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
      });
      toast('已开始执行');
    } catch (e) {
      if (e.status === 409) toast('该动作正在运行中');
      else if (e.status === 429) toast('请求过于频繁');
      else toast('执行失败');
      runBtn.disabled = false;
      runBtn.textContent = '执行';
      stopBtn.style.display = 'none';
      return;
    }

    // 轮询：直到脚本不再 running，刷新日志显示最新结果
    let timer = null;
    const startedAt = Date.now();
    let busy = false;
    const poll = async () => {
      // 轮询所属按钮已随 DOM 移除时立即注销，避免隐藏页/离开页面后空转。
      if (!document.body.contains(runBtn)) { clearInterval(timer); return; }
      if (document.hidden || busy) return;
      if (Date.now() - startedAt > 300000) {
        clearInterval(timer);
        runBtn.disabled = false;
        runBtn.textContent = '执行';
        stopBtn.style.display = 'none';
        toast('执行状态轮询超时，请手动刷新');
        return;
      }
      busy = true;
      try {
        await refreshLogs();
        const cur = await fetchJSON('/api/actions/' + actionId);
        // 后端 action_state 只返回 running / idle，'queued' 分支是死代码。
        if (cur.state !== 'running') {
          // 结束
          clearInterval(timer);
          runBtn.disabled = false;
          runBtn.textContent = '执行';
          stopBtn.style.display = 'none';
          await refreshLogs();
        }
      } catch (e) {
        // 网络错误时也停止轮询，避免死循环
        clearInterval(timer);
        runBtn.disabled = false;
        runBtn.textContent = '执行';
        stopBtn.style.display = 'none';
      } finally {
        busy = false;
      }
    };
    timer = setInterval(poll, 1000);
    window.addEventListener('pagehide', () => clearInterval(timer), { once: true });
    if (!document.hidden) poll();
  }

  // ============ 日志页 ============
  async function loadLogs() {
    const root = document.getElementById('logs-root');
    if (!root) return;
    // 从 URL 读取 action_id 参数（从脚本卡片的"查看日志"跳转而来）
    const params = new URLSearchParams(window.location.search);
    const actionId = params.get('action_id') || '';
    let url = API.logs + '?limit=50';
    if (actionId) url += '&action_id=' + encodeURIComponent(actionId);
    try {
      const data = await fetchJSON(url);
      renderLogs(root, data);
    } catch (e) {
      root.innerHTML = '<div class="muted">加载失败</div>';
    }
  }

  function renderLogs(root, data) {
    root.innerHTML = '';
    const title = document.createElement('h1');
    title.textContent = '日志';
    root.appendChild(title);

    // 筛选器：组下拉 + 触发方式下拉
    const filters = document.createElement('div');
    filters.className = 'filters';
    const groupSel = document.createElement('select');
    groupSel.innerHTML = '<option value="">全部组</option>';
    const triggerSel = document.createElement('select');
    triggerSel.innerHTML = '<option value="">全部触发</option><option value="manual">手动</option><option value="schedule">定时</option>';
    // 收集所有组名
    const groups = new Set();
    (data.items || []).forEach((e) => groups.add(e.action_group));
    groups.forEach((g) => {
      const opt = document.createElement('option');
      opt.value = g; opt.textContent = g;
      groupSel.appendChild(opt);
    });
    filters.appendChild(groupSel);
    filters.appendChild(triggerSel);
    root.appendChild(filters);

    // 表格容器
    const tableWrap = document.createElement('div');
    tableWrap.style.cssText = 'overflow-x:auto;';
    root.appendChild(tableWrap);

    let beforeId = data.next_before_id || null;
    const hasMore = data.has_more;

    const applyFilter = async () => {
      const g = groupSel.value;
      const t = triggerSel.value;
      let url = API.logs + '?limit=50';
      if (g) url += '&group=' + encodeURIComponent(g);
      if (t) url += '&trigger=' + t;
      try {
        const d = await fetchJSON(url);
        beforeId = d.next_before_id || null;
        renderTable(tableWrap, d.items || []);
        loadMoreBtn.style.display = d.has_more ? 'block' : 'none';
      } catch (e) {
        console.warn('日志筛选加载失败', e);
        toast('日志筛选失败，请重试');
      }
    };
    groupSel.addEventListener('change', applyFilter);
    triggerSel.addEventListener('change', applyFilter);

    const renderTable = (wrap, items) => {
      wrap.innerHTML = '';
      const table = document.createElement('table');
      table.className = 'logs';
      const thead = document.createElement('thead');
      thead.innerHTML =
        '<tr><th>时间</th><th>脚本</th><th>组</th><th>版本</th><th>触发</th><th>推送</th><th>状态</th><th>耗时</th></tr>';
      table.appendChild(thead);
      const tbody = document.createElement('tbody');
      items.forEach((e) => {
        const tr = document.createElement('tr');
        const isOp = e.kind && e.kind !== 'run';
        const kindLabel = { create: '新增', update: '编辑', delete: '删除' }[e.kind] || '';
        const trigger = e.trigger === 'manual' ? '手动' : '定时';
        const push = e.push_status === 'ok' ? '✓' : (e.push_status && e.push_status.failed ? '⚠' : '—');
        const status = isOp ? (kindLabel || '操作') : (e.exit_code === 0 ? '✓ 0' : '✗ ' + (e.exit_code === null ? '-' : escapeHtml(e.exit_code)));
        const dur = isOp ? '—' : (e.duration_ms / 1000).toFixed(1) + 's';
        const titleCell = isOp ? ('[' + kindLabel + '] ' + escapeHtml(e.action_title)) : (escapeHtml(e.action_title) + ' <span class="muted" style="font-size:11px;">(' + escapeHtml(e.action_id) + ')</span>');
        tr.innerHTML =
          '<td>' + escapeHtml(e.started_at_local || e.started_at) + '</td>' +
          '<td>' + titleCell + '</td>' +
          '<td>' + escapeHtml(e.action_group) + '</td>' +
          '<td>' + escapeHtml(e.action_version || '—') + '</td>' +
          '<td>' + trigger + '</td>' +
          '<td>' + push + '</td>' +
          '<td>' + status + '</td>' +
          '<td>' + dur + '</td>';
        tr.addEventListener('click', () => {
          const detail = document.createElement('tr');
          detail.innerHTML = '<td colspan="8"><pre style="white-space:pre-wrap;word-break:break-all">' +
            escapeHtml((e.stdout || '') + (e.stderr ? '\n--- stderr ---\n' + e.stderr : '')) + '</pre></td>';
          if (tr.nextSibling && tr.nextSibling.dataset.detail) {
            tr.nextSibling.remove();
          } else {
            detail.dataset.detail = '1';
            tr.after(detail);
          }
        });
        tbody.appendChild(tr);
      });
      table.appendChild(tbody);
      wrap.appendChild(table);
    };

    renderTable(tableWrap, data.items || []);

    // 加载更多
    const loadMoreBtn = document.createElement('button');
    loadMoreBtn.className = 'secondary';
    loadMoreBtn.textContent = '加载更多';
    loadMoreBtn.style.cssText = 'margin-top:12px;width:100%;';
    loadMoreBtn.style.display = hasMore ? 'block' : 'none';
    loadMoreBtn.addEventListener('click', async () => {
      if (beforeId === null) return;
      const g = groupSel.value;
      const t = triggerSel.value;
      let url = API.logs + '?limit=50&before_id=' + beforeId;
      if (g) url += '&group=' + encodeURIComponent(g);
      if (t) url += '&trigger=' + t;
      try {
        const d = await fetchJSON(url);
        beforeId = d.next_before_id || null;
        // 追加到现有表格
        const wrap = tableWrap;
        const table = wrap.querySelector('table');
        const tbody = table && table.querySelector('tbody');
        if (!tbody) { toast('日志列表已失效，请重新筛选'); return; }
        // 离屏批量构建后一次挂载，避免逐行 reflow；同时限制表格总行数。
        const frag = document.createDocumentFragment();
        const room = Math.max(0, 500 - tbody.children.length);
        const batch = (d.items || []).slice(0, room);
        batch.forEach((e) => {
          const tr = document.createElement('tr');
          // 与日志页主表保持一致：操作日志不显示成错误退出码。
          const isOp = e.kind && e.kind !== 'run';
          const kindLabel = { create: '新增', update: '编辑', delete: '删除' }[e.kind] || '';
          const trigger = e.trigger === 'manual' ? '手动' : '定时';
          const push = e.push_status === 'ok' ? '✓' : (e.push_status && e.push_status.failed ? '⚠' : '—');
          const status = isOp ? (kindLabel || '操作') : (e.exit_code === 0 ? '✓ 0' : '✗ ' + (e.exit_code === null ? '-' : escapeHtml(e.exit_code)));
          const dur = isOp ? '—' : (e.duration_ms / 1000).toFixed(1) + 's';
          tr.innerHTML =
            '<td>' + escapeHtml(e.started_at_local || e.started_at) + '</td>' +
            '<td>' + escapeHtml(e.action_title) + '</td>' +
            '<td>' + escapeHtml(e.action_group) + '</td>' +
            '<td>' + escapeHtml(e.action_version || '—') + '</td>' +
            '<td>' + trigger + '</td>' +
            '<td>' + push + '</td>' +
            '<td>' + status + '</td>' +
            '<td>' + dur + '</td>';
          tr.addEventListener('click', () => {
            const detail = document.createElement('tr');
            detail.innerHTML = '<td colspan="8"><pre style="white-space:pre-wrap;word-break:break-all">' +
              escapeHtml((e.stdout || '') + (e.stderr ? '\n--- stderr ---\n' + e.stderr : '')) + '</pre></td>';
            if (tr.nextSibling && tr.nextSibling.dataset.detail) {
              tr.nextSibling.remove();
            } else {
              detail.dataset.detail = '1';
              tr.after(detail);
            }
          });
          frag.appendChild(tr);
        });
        tbody.appendChild(frag);
        if ((d.items || []).length > room) toast('已达 500 条显示上限，请缩小筛选条件');
        loadMoreBtn.style.display = d.has_more && room > 0 ? 'block' : 'none';
      } catch (e) {
        console.warn('日志加载更多失败', e);
        toast('加载更多失败，请重试');
      }
    });
    root.appendChild(loadMoreBtn);
  }

  // ============ 设置页 ============
  // 传空标题时不生成 card-title：由分区标题（h2）承担唯一一级标题，避免两级标题重复
  function makeCard(titleText) {
    const c = document.createElement('div');
    c.className = 'card';
    if (titleText) {
      const h = document.createElement('div');
      h.className = 'card-title';
      h.textContent = titleText;
      c.appendChild(h);
    }
    return c;
  }

  // 下拉设置项：options 为 [value, 显示文本] 数组
  function makeSelect(label, key, options, current) {
    const div = document.createElement('div');
    div.className = 'field';
    const lbl = document.createElement('label');
    lbl.textContent = label;
    div.appendChild(lbl);
    const sel = document.createElement('select');
    sel.dataset.setting = key;
    options.forEach(([v, t]) => {
      const o = document.createElement('option');
      o.value = v; o.textContent = t;
      if (v === current) o.selected = true;
      sel.appendChild(o);
    });
    div.appendChild(sel);
    return div;
  }

  // 卡片内逻辑子区标题：跨两列强制另起一行，把不同层级的设置项分段。
  // 互斥字段切换时，后续子区始终从固定行开始，避免网格跳位。
  function makeSubhead(text) {
    const d = document.createElement('div');
    d.className = 'settings-subhead';
    d.textContent = text;
    return d;
  }

  // 凭据配置状态徽标：状态不再拼进标签文字，统一用右侧徽标展示
  function makeStatusBadge(configured) {
    const b = document.createElement('span');
    b.className = 'settings-badge' + (configured ? ' configured' : '');
    b.textContent = configured ? '已配置' : '未配置';
    return b;
  }

  // 标签内的小字补充说明（如「逗号分隔」），不占用独立行
  function makeHintInline(text) {
    const s = document.createElement('span');
    s.className = 'settings-label-hint';
    s.textContent = text;
    return s;
  }

  // 卡片级开关行：跨两列独立成行，左侧标题+说明、右侧复选框；整行 label 可点击
  // 带单位后缀的数字输入：单位紧贴输入框右侧，不再写进 label
  function fieldWithSuffix(label, value, key, suffix) {
    const div = document.createElement('div');
    div.className = 'field field-suffix';
    const lbl = document.createElement('label');
    lbl.textContent = label;
    const wrap = document.createElement('div');
    wrap.className = 'suffix-wrap';
    const input = document.createElement('input');
    input.type = 'number';
    input.dataset.setting = key;
    input.value = value === null || value === undefined ? '' : value;
    const sfx = document.createElement('span');
    sfx.className = 'suffix';
    sfx.textContent = suffix;
    wrap.appendChild(input);
    wrap.appendChild(sfx);
    div.appendChild(lbl);
    div.appendChild(wrap);
    return div;
  }

  function makeSwitchRow(title, desc, checked, key) {
    const row = document.createElement('label');
    row.className = 'settings-switch-row';
    const text = document.createElement('span');
    text.className = 'switch-text';
    const t = document.createElement('span');
    t.className = 'switch-title';
    t.textContent = title;
    text.appendChild(t);
    const d = document.createElement('span');
    d.className = 'switch-desc';
    d.textContent = desc;
    text.appendChild(d);
    const input = document.createElement('input');
    input.type = 'checkbox';
    input.checked = !!checked;
    input.dataset.setting = key;
    row.appendChild(text);
    row.appendChild(input);
    return row;
  }

  // 说明块：占网格一列、与同排输入框底部对齐，避免切换通道时留下半行空白
  function makeNoteField(text) {
    const d = document.createElement('div');
    d.className = 'field settings-note-field';
    const box = document.createElement('div');
    box.className = 'settings-note-box';
    box.textContent = text;
    d.appendChild(box);
    return d;
  }

  // 区块说明：紧跟所属字段组，统一 12px 小字
  function makeHintLine(text) {
    return el('div', { class: 'settings-hint' }, text);
  }

  async function renderSettings() {
    const root = document.getElementById('settings-root');
    if (!root) return;
    if (usageTimer) { clearInterval(usageTimer); usageTimer = null; }
    usageRefresh = null;
    root.innerHTML = '';
    // 页面标题行：标题 + 保存按钮（右上角，设置项再多也不用滚到底部）
    const head = document.createElement('div');
    head.className = 'page-head';
    const title = document.createElement('h1');
    title.textContent = '设置';
    const saveBtn = document.createElement('button');
    saveBtn.id = 'settings-save';
    saveBtn.className = 'page-head-action';
    saveBtn.textContent = '保存设置';
    head.appendChild(title);
    head.appendChild(saveBtn);
    root.appendChild(head);

    /** @type {SettingsResponse} */
    let settings;
    try {
      /** @type {SettingsResponse} */
      const data = await fetchJSON('/api/settings');
      settings = data;
      // 与动作页共用界面设置缓存，避免设置页 → 动作页重复请求 /api/settings。
      uiSettings = data;
    } catch (e) {
      root.appendChild(el('div', { class: 'muted' }, '加载设置失败'));
      return;
    }

    // 按信息层级分区：同一分区内卡片双列排布，窄屏自动单列
    const makeSection = (heading) => {
      const sec = document.createElement('section');
      sec.className = 'settings-section';
      const h2 = document.createElement('h2');
      h2.textContent = heading;
      sec.appendChild(h2);
      const grid = document.createElement('div');
      grid.className = 'settings-grid';
      sec.appendChild(grid);
      root.appendChild(sec);
      return grid;
    };

    // ===== 安全与访问 =====
    const secGrid = makeSection('安全与访问');
    const secCard = makeCard('Token 认证');
    secCard.appendChild(fieldRow('启用安全模式', settings.auth_enabled, 'auth_enabled', false, true));
    secCard.appendChild(el('div', { class: 'meta-line' },
      '开启后需 /?token=xxx 登录；由关切换为开时自动生成新 Token，仅显示一次。'));
    const resetBtn = document.createElement('button');
    resetBtn.textContent = '重置 Token';
    resetBtn.className = 'danger';
    if (!settings.auth_enabled) {
      resetBtn.disabled = true;
      resetBtn.title = '请先开启安全模式';
    }
    resetBtn.addEventListener('click', () => {
      if (!confirm('重置 Token 后所有设备需重新登录，新 Token 仅显示一次。确认重置？')) return;
      fetchJSON('/api/auth/reset-token', { method: 'POST' })
        .then((resp) => showNewToken(resp.token))
        .catch((err) => toast('重置失败：' + ((err.body && err.body.message) || '请检查安全模式是否开启')));
    });
    secCard.appendChild(resetBtn);
    secGrid.appendChild(secCard);

    // ===== 运行与存储 =====
    const runGrid = makeSection('运行与存储');
    const logCard = makeCard('');
    // 标题行：标题 + 单位段控。单位只影响显示、不是持久化设置，
    // 放在标题行右侧比单独占一列输入区更省空间，也不会让「MB」出现两次。
    const logHead = document.createElement('div');
    logHead.className = 'card-title-row';
    const logTitle = document.createElement('div');
    logTitle.className = 'card-title';
    logTitle.textContent = '日志';
    const logTools = document.createElement('div');
    logTools.className = 'card-tools';
    const seg = document.createElement('div');
    seg.className = 'seg';
    // 单位是显示偏好，存本地：面板重启 / 重新打开设置页都保持上次选择
    let usageUnit = localStorage.getItem('olivetin.logUnit') === 'KB' ? 'KB' : 'MB';
    const segBtns = {};
    [['KB', 'KB'], ['MB', 'MB']].forEach(([v]) => {
      const b = document.createElement('button');
      b.type = 'button';
      b.textContent = v;
      b.className = v === usageUnit ? 'active' : '';
      segBtns[v] = b;
      seg.appendChild(b);
    });
    logTools.appendChild(seg);
    logHead.appendChild(logTitle);
    logHead.appendChild(logTools);
    logCard.appendChild(logHead);

    // 占用块：数字 + 单位紧跟其后（同一基线），右侧刷新
    let usageBytes = Number(settings.log_usage_bytes || 0);
    let usageFiles = Number(settings.log_usage_files || 0);
    const usageBlock = document.createElement('div');
    usageBlock.className = 'usage-block';
    const usageNum = el('span', { class: 'usage-number' }, '—');
    const usageUnitEl = el('span', { class: 'usage-unit' }, usageUnit);
    const usageSub = el('span', { class: 'usage-sub' }, '');
    const clearBtn = document.createElement('button');
    clearBtn.className = 'btn-quiet-danger usage-refresh';
    clearBtn.textContent = '清除全部日志文件';
    clearBtn.title = '删除日志目录下的所有 .log 文件（不可恢复）';
    usageBlock.appendChild(usageNum);
    usageBlock.appendChild(usageUnitEl);
    usageBlock.appendChild(usageSub);
    usageBlock.appendChild(clearBtn);
    logCard.appendChild(usageBlock);

    const renderUsage = () => {
      const divisor = usageUnit === 'MB' ? 1024 * 1024 : 1024;
      const n = usageBytes / divisor;
      usageNum.textContent = n >= 0.01 ? n.toFixed(2) : (usageBytes > 0 ? '<0.01' : '0.00');
      usageUnitEl.textContent = usageUnit;
      usageSub.textContent = usageFiles + ' 个文件';
      usageNum.title = usageBytes + ' B';
    };
    Object.keys(segBtns).forEach((v) => {
      segBtns[v].addEventListener('click', () => {
        usageUnit = v;
        localStorage.setItem('olivetin.logUnit', v);
        Object.keys(segBtns).forEach((k) => {
          segBtns[k].className = k === v ? 'active' : '';
        });
        renderUsage();
      });
    });
    renderUsage();

    const refreshUsage = async () => {
      try {
        const s = await fetchJSON('/api/settings');
        usageBytes = Number(s.log_usage_bytes || 0);
        usageFiles = Number(s.log_usage_files || 0);
        renderUsage();
      } catch (e) { toast('日志占用刷新失败'); }
    };
    const fmtBytes = (b) => (b >= 1048576
      ? (b / 1048576).toFixed(2) + ' MB'
      : b >= 1024 ? (b / 1024).toFixed(1) + ' KB' : b + ' B');
    clearBtn.addEventListener('click', async () => {
      const dir = settings.log_directory || '日志目录';
      const okConfirm = confirm(
        '确定清除全部日志文件？\n\n' +
        '将删除 ' + dir + ' 下的所有 .log 文件（面板日志、脚本日志、总日志），不可恢复。\n' +
        '日志页的内存记录也会一并清空，清除后列表立即为空。'
      );
      if (!okConfirm) return;
      clearBtn.disabled = true;
      try {
        const r = await fetchJSON('/api/logs/clear', { method: 'POST' });
        const n = Number(r.removed || 0);
        const c = Number(r.cleared_entries || 0);
        toast('已删除 ' + n + ' 个日志文件、清空 ' + c + ' 条内存记录，释放 ' + fmtBytes(Number(r.freed_bytes || 0)));
        if (r.errors && r.errors.length) toast('部分文件删除失败：' + r.errors[0]);
        await refreshUsage();
      } catch (e) {
        if (!e || e.status !== 403) toast((e.body && (e.body.message || e.body.error)) || '清除失败');
      } finally {
        clearBtn.disabled = false;
      }
    });
    // 日志持续增长时数值保持准确：页面可见时每 30 秒重统计；
    // 后端 /api/settings 的占用统计已有 30 秒 TTL 缓存，不会再每请求全目录 stat。
    usageRefresh = refreshUsage;
    usageTimer = setInterval(() => {
      if (document.hidden) return;
      if (!document.body.contains(usageNum)) {
        clearInterval(usageTimer);
        usageTimer = null;
        usageRefresh = null;
        return;
      }
      refreshUsage();
    }, 30000);

    const dirRow = fieldRow('日志目录', String(settings.log_directory || ''), 'log_directory', true);
    dirRow.classList.add('field-wide');
    logCard.appendChild(dirRow);
    // 单位紧跟数字之后：输入框内右侧贴一个后缀，比写进 label 更直观
    logCard.appendChild(fieldWithSuffix('保留天数', settings.log_retention_days === null ? 0 : settings.log_retention_days, 'log_retention_days', '天'));
    logCard.appendChild(fieldWithSuffix('总大小上限', settings.log_max_total_mb, 'log_max_total_mb', 'MB'));
    logCard.appendChild(el('div', { class: 'meta-line' },
      (settings.log_usage_rule || '1 KB = 1024 B，1 MB = 1024 KB') + '；保留天数填 0 不清理，总大小上限填 0 不限制。'));
    runGrid.appendChild(logCard);

    const verCard = makeCard('版本管理');
    verCard.appendChild(makeSwitchRow('允许版本管理',
      '关闭后所有脚本都不再使用版本目录，面板内也不能上传新版本',
      settings.versioning_enabled !== false, 'versioning_enabled'));
    verCard.appendChild(fieldRow('可回退版本数量（1~50）', settings.max_versions, 'max_versions'));
    verCard.appendChild(el('div', { class: 'meta-line' }, '超出上限自动清理最旧版本，活跃版本不会被删除。'));
    runGrid.appendChild(verCard);

    // ===== 界面 · 分组列表 =====
    // 层级：显示方式（外观配置）→ 分组顺序（就地管理入口），不再把操作甩到动作页
    const uiGrid = makeSection('界面 · 分组列表');
    const uiCard = makeCard('');
    uiCard.appendChild(makeSubhead('显示方式'));
    uiCard.appendChild(makeSelect('分组面板位置', 'group_panel_position',
      [['top', '顶部'], ['left', '左侧侧栏'], ['right', '右侧侧栏']],
      settings.group_panel_position || 'top'));
    uiCard.appendChild(makeSelect('组内脚本排列', 'group_layout',
      [['vertical', '纵向'], ['horizontal', '横向']],
      settings.group_layout || 'vertical'));
    uiCard.appendChild(fieldRow(
      '默认收起分组', settings.collapse_groups_by_default, 'collapse_groups_by_default', false, true));

    uiCard.appendChild(makeSubhead('分组顺序'));
    const manageBtn = document.createElement('button');
    manageBtn.className = 'secondary';
    manageBtn.textContent = '管理分组与顺序';
    manageBtn.addEventListener('click', openGroupManager);
    uiCard.appendChild(manageBtn);
    uiCard.appendChild(el('div', { class: 'meta-line' },
      '新增、重命名、删除分组，以及用 ↑ ↓ 调整先后顺序，均在此完成。'));
    uiGrid.appendChild(uiCard);

    // ===== 通知 · WxPusher =====
    // 结构：卡片级总开关 → 通道配置 → 推送行为。
    // 复选框独立成行，凭据区切换通道时始终占满两列；所有间距由 CSS 变量统一控制。
    const pushGrid = makeSection('通知 · WxPusher');
    const pushCard = makeCard('');
    pushCard.classList.add('push-card');
    const p = settings.push;

    // ① 总开关：决定整张卡片是否生效，与具体配置项拉开层级
    pushCard.appendChild(makeSwitchRow(
      '启用推送', '关闭后保留全部配置，但不发送任何通知', p.enabled, 'push_enabled'));

    // ② 通道配置：选类型 → 填对应 Token → 区块说明
    pushCard.appendChild(makeSubhead('通道配置'));
    const typeDiv = document.createElement('div');
    typeDiv.className = 'field';
    const typeLbl = document.createElement('label');
    typeLbl.textContent = '推送通道';
    typeDiv.appendChild(typeLbl);
    // 通道仅决定显示哪套凭据，不是持久化设置项：不挂 data-setting，避免混进保存字段
    const typeSel = document.createElement('select');
    typeSel.id = 'push-channel';
    [['spt', '极简推送（SPT Token）'], ['app', '应用推送（App Token）']].forEach(([v, t]) => {
      const o = document.createElement('option');
      o.value = v; o.textContent = t;
      typeSel.appendChild(o);
    });
    typeSel.value = p.app_token_configured && !p.spt_configured ? 'app' : 'spt';
    typeDiv.appendChild(typeSel);
    pushCard.appendChild(typeDiv);
    pushCard.appendChild(makeNoteField('极简推送只需 SPT Token；应用推送需 App Token 与接收者 UIDS。'));

    const sptInitial = p.spt || '';
    const appInitial = p.app_token || '';
    const sptDiv = fieldRow('SPT Token', sptInitial, 'push_spt');
    sptDiv.querySelector('label').appendChild(makeStatusBadge(p.spt_configured));
    const sptNote = makeNoteField('极简通道仅校验 SPT Token，不需要接收者 UIDS。');
    const appTokenDiv = fieldRow('App Token', appInitial, 'push_app_token');
    appTokenDiv.querySelector('label').appendChild(makeStatusBadge(p.app_token_configured));
    const uidsDiv = fieldRow('接收者 UIDS', p.uids || '', 'push_uids');
    uidsDiv.querySelector('label').appendChild(makeHintInline('逗号分隔'));
    pushCard.appendChild(sptDiv);
    pushCard.appendChild(sptNote);
    pushCard.appendChild(appTokenDiv);
    pushCard.appendChild(uidsDiv);

    const syncPushType = () => {
      const isSpt = typeSel.value === 'spt';
      sptDiv.style.display = isSpt ? '' : 'none';
      sptNote.style.display = isSpt ? '' : 'none';
      appTokenDiv.style.display = isSpt ? 'none' : '';
      uidsDiv.style.display = isSpt ? 'none' : '';
    };
    typeSel.addEventListener('change', syncPushType);
    syncPushType();
    pushCard.appendChild(makeHintLine(
      '凭据脱敏显示，未修改不会覆盖；留空并保存即清空该凭据。'));

    // ③ 推送行为：只描述「推什么」，与「用哪条通道」分开
    pushCard.appendChild(makeSubhead('推送行为'));
    // 整行项：与下方 switch-row 同为「一行一件事」，不再独占半列留白
    const sumField = makeSelect('通知栏标题', 'push_summary_mode', [
      ['auto', '默认：前缀 + 脚本名 + 耗时'],
      ['content', '取正文首行'],
      ['none', '不传标题'],
    ], ['auto', 'content', 'none'].indexOf(p.summary_mode) >= 0 ? p.summary_mode : 'auto');
    sumField.classList.add('field-inline');
    pushCard.appendChild(sumField);
    pushCard.appendChild(makeSwitchRow(
      '推送包含脚本输出', '仅在未设置正文模板时生效', p.include_output, 'push_include_output'));
    pushCard.appendChild(makeHintLine(
      '保存时若已启用推送，会自动发送一条测试推送用于验证通道。'));
    pushGrid.appendChild(pushCard);

    // ===== 保存（按钮在页面右上角，这里只绑定行为）=====
    saveBtn.addEventListener('click', async () => {
      const getVal = (key) => {
        const el = root.querySelector('[data-setting="' + key + '"]');
        if (!el) return undefined;
        if (el.type === 'checkbox') return el.checked;
        return el.value;
      };
      // 后端校验为唯一权威（后端 Q6/Q7）：范围问题只提示、原值照发；
      // 空值 / 非数字等格式问题仍回落默认值（这属于格式校验，不是范围校验）。
      /**
       * @param {string} key
       * @param {number} min
       * @param {number} max
       * @param {number} fallback
       * @param {string} label
       * @returns {number}
       */
      const parseIntField = (key, min, max, fallback, label) => {
        const raw = String(getVal(key) === undefined ? '' : getVal(key)).trim();
        if (!/^\d+$/.test(raw)) {
          toast(label + ' 不是合法整数，已按默认值 ' + fallback + ' 提交');
          return fallback;
        }
        const n = Number(raw);
        if (n < min || n > max) {
          toast(label + ' 超出建议范围 ' + min + '~' + max + '（当前 ' + n + '），已按原值提交，由服务端决定');
        }
        return n;
      };
      let body;
      try {
        body = {
          // 格式问题回落的默认值对齐 src/config.rs 的 default 函数：7 / 20 / 5。
          log_retention_days: parseIntField('log_retention_days', 0, 3650, 7, '日志保留天数'),
          log_max_total_mb: parseIntField('log_max_total_mb', 0, 100000, 20, '日志总大小上限'),
          max_versions: parseIntField('max_versions', 1, 50, 5, '可回退版本数量'),
          versioning_enabled: getVal('versioning_enabled'),
          push_enabled: getVal('push_enabled'),
          push_include_output: getVal('push_include_output'),
          push_summary_mode: getVal('push_summary_mode'),
          auth_enabled: getVal('auth_enabled'),
          collapse_groups_by_default: getVal('collapse_groups_by_default'),
          group_layout: getVal('group_layout'),
          group_panel_position: getVal('group_panel_position'),
          push_uids: getVal('push_uids'),
        };
        // 仅在用户确实改动凭据时才提交（脱敏值原样不动 = 不写回）
        const sptVal = getVal('push_spt');
        const appVal = getVal('push_app_token');
        if (sptVal !== sptInitial) body.push_spt = sptVal;
        if (appVal !== appInitial) body.push_app_token = appVal;
      } catch (err) {
        toast(err.message);
      }
      try {
        const resp = await fetchJSON('/api/settings', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        });
        uiSettings = null; // 动作页下次进入重新拉取界面设置
        toast('设置已保存并生效');
        if (body.push_enabled) {
          try {
            await fetchJSON('/api/push/test', { method: 'POST' });
            toast('测试推送已发送，请查收微信通知');
          } catch (te) {
            toast('保存成功，但测试推送失败：' + ((te.body && te.body.message) || '通道不可用'));
          }
        }
        if (resp && resp.new_token) showNewToken(resp.new_token);
        renderSettings(); // 刷新安全模式开关状态
      } catch (e) {
        if (!e || e.status !== 403) toast('保存失败：' + ((e.body && e.body.message) || (e.body && e.body.error) || '请检查配置'));
      }
    });


    // ===== 关于：版本 / commit / 构建时间 =====
    // 顶栏只放短版本号，这里给全量信息，排查问题时可以直接对照。
    const aboutGrid = makeSection('关于');
    const aboutCard = makeCard('');
    aboutCard.classList.add('about-card');
    // 「关于」不是表单：用定义列表（标签右对齐 + 等宽值），
    // 比一排像输入框的只读盒子更像系统关于窗口，也不占满整行宽度。
    const aboutHead = document.createElement('div');
    aboutHead.className = 'card-title-row';
    const aboutTitle = document.createElement('div');
    aboutTitle.className = 'card-title';
    aboutTitle.textContent = '版本信息';
    const aboutTools = document.createElement('div');
    aboutTools.className = 'card-tools';
    const verBadge = el('span', { class: 'badge' }, 'v—');
    aboutTools.appendChild(verBadge);
    aboutHead.appendChild(aboutTitle);
    aboutHead.appendChild(aboutTools);
    aboutCard.appendChild(aboutHead);

    const dl = document.createElement('dl');
    dl.className = 'about-list';
    const addRow = (k) => {
      const dt = document.createElement('dt');
      dt.textContent = k;
      const dd = document.createElement('dd');
      dd.textContent = '—';
      dl.appendChild(dt);
      dl.appendChild(dd);
      return dd;
    };
    const dCommit = addRow('commit');
    const dBuilt = addRow('构建时间');
    const dRustc = addRow('编译器');
    aboutCard.appendChild(dl);
    const fill = (node, text) => {
      node.textContent = text || 'unknown';
      node.title = text || '';
    };
    try {
      const v = await fetchJSON('/api/version');
      verBadge.textContent = v.version;
      fill(dCommit, v.git_commit);
      fill(dBuilt, v.built_at);
      fill(dRustc, v.rustc);
    } catch (e) {
      verBadge.textContent = '读取失败';
    }
    aboutCard.appendChild(el('div', { class: 'meta-line about-note' },
      '与顶栏徽标、olivetin-rs --version 输出一致；值可选中复制。'));
    aboutGrid.appendChild(aboutCard);
  }

  function fieldRow(label, value, key, readonly, isCheckbox) {
    const div = document.createElement('div');
    div.className = 'field' + (isCheckbox ? ' check-field' : '');
    const lbl = document.createElement('label');
    lbl.textContent = label;
    if (isCheckbox) {
      // 复选框与文字同行，行高与输入型控件一致；顺序保持一致（方块在前）
      const input = document.createElement('input');
      input.type = 'checkbox';
      input.checked = !!value;
      input.dataset.setting = key;
      lbl.insertBefore(input, lbl.firstChild);
      div.appendChild(lbl);
      return div;
    }
    div.appendChild(lbl);
    const input = document.createElement('input');
    input.type = 'text';
    input.value = value === null || value === undefined ? '' : String(value);
    if (readonly) input.readOnly = true;
    input.dataset.setting = key;
    div.appendChild(input);
    return div;
  }

  function el(tag, attrs, text) {
    const e = document.createElement(tag);
    if (attrs) for (const k in attrs) e.setAttribute(k, attrs[k]);
    if (text !== undefined) e.textContent = text;
    return e;
  }

  function showNewToken(token) {
    const dialog = document.createElement('dialog');
    dialog.innerHTML =
      '<div class="dialog-header"><span class="dialog-title">新 Token</span></div>' +
      '<textarea id="tok-val" readonly rows="3" class="mono" style="width:100%;word-break:break-all;margin-bottom:12px;padding:8px;border:1px solid var(--border);border-radius:6px;background:var(--card);color:var(--text);"></textarea>' +
      '<div class="version-actions">' +
      '<button id="tok-copy">复制</button>' +
      '<button class="secondary" id="tok-close">关闭</button>' +
      '</div>' +
      '<div class="muted" style="margin-top:12px">仅显示一次，请立即保存。其他设备需访问 /?token=新token 登录。</div>';
    document.body.appendChild(dialog);
    const tokVal = dialog.querySelector('#tok-val');
    tokVal.value = token;
    dialog.showModal();
    dialog.querySelector('#tok-copy').addEventListener('click', async () => {
      const ok = await copyText(token);
      if (ok) {
        toast('已复制，请妥善保存');
      } else {
        // 自动复制失败时仍可手动全选复制，不再静默无反馈
        tokVal.focus();
        tokVal.select();
        toast('自动复制失败，已选中文本，请按 Ctrl+C 手动复制');
      }
    });
    dialog.querySelector('#tok-close').addEventListener('click', () => dialog.close());
  }

  // ============ 初始化 ============
  function init() {
    const path = window.location.pathname;
    if (path === '/' || path === '') loadActions();
    else if (path === '/logs') loadLogs();
    else if (path === '/settings') renderSettings();
    else if (path.startsWith('/action/')) {
      const id = path.replace('/action/', '');
      loadActionDetail(id);
    }
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();

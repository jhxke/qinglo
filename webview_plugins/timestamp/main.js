// 时间戳转换外部插件：纯前端工具，无需 Rust IPC。
// 功能：当前时间戳实时显示 / 时间戳→日期（秒、毫秒自动识别）/ 日期→时间戳。
// 日期输入支持 datetime-local 选择器与常见文本格式（YYYY-MM-DD[ HH:mm[:ss]]、
// 斜杠日期、ISO 8601 含时区 Z / +08:00 等）。
(function () {
  var $ = function (id) { return document.getElementById(id); };
  var WEEKS = ['日', '一', '二', '三', '四', '五', '六'];

  function pad(n) { return (n < 10 ? '0' : '') + n; }
  function pad3(n) { return n < 10 ? '00' + n : (n < 100 ? '0' + n : '' + n); }

  // ===== 格式化 =====
  function fmtLocal(ms) {
    var d = new Date(ms);
    return d.getFullYear() + '-' + pad(d.getMonth() + 1) + '-' + pad(d.getDate())
      + ' ' + pad(d.getHours()) + ':' + pad(d.getMinutes()) + ':' + pad(d.getSeconds());
  }
  function fmtUtc(ms) {
    var d = new Date(ms);
    return d.getUTCFullYear() + '-' + pad(d.getUTCMonth() + 1) + '-' + pad(d.getUTCDate())
      + ' ' + pad(d.getUTCHours()) + ':' + pad(d.getUTCMinutes()) + ':' + pad(d.getUTCSeconds());
  }
  function tzOffsetLabel(d) {
    // 东八区 getTimezoneOffset() = -480，显示为 UTC+08:00
    var off = -d.getTimezoneOffset();
    var sign = off >= 0 ? '+' : '-';
    var a = Math.abs(off);
    return 'UTC' + sign + pad(Math.floor(a / 60)) + ':' + pad(a % 60);
  }
  function relTime(ms) {
    var diff = ms - Date.now();
    var abs = Math.abs(diff);
    if (abs < 1000) return '此刻';
    var units = [
      ['年', 365 * 24 * 3600 * 1000],
      ['个月', 30 * 24 * 3600 * 1000],
      ['天', 24 * 3600 * 1000],
      ['小时', 3600 * 1000],
      ['分钟', 60 * 1000],
      ['秒', 1000]
    ];
    for (var i = 0; i < units.length; i++) {
      if (abs >= units[i][1]) {
        return Math.floor(abs / units[i][1]) + units[i][0] + (diff > 0 ? '后' : '前');
      }
    }
  }

  // ===== 输出辅助 =====
  function line(label, id, value, mono) {
    return '<div class="p-ts-line"><b>' + label + '</b>'
      + '<span' + (mono ? ' class="p-ts-mono"' : '') + ' id="' + id + '">'
      + value + '</span>'
      + '<button class="p-ts-copy" type="button" data-copy-id="' + id + '">复制</button></div>';
  }
  function setHint(out, msg) {
    out.className = 'p-ts-out';
    out.innerHTML = '<div class="p-ts-hint">' + msg + '</div>';
  }
  function setErr(out, msg) {
    out.className = 'p-ts-out';
    out.innerHTML = '<div class="p-ts-err">' + msg + '</div>';
  }

  // ===== 时间戳 → 日期 =====
  function renderTs() {
    var out = $('p-ts-ts-out');
    var raw = $('p-ts-ts-input').value.trim();
    if (!raw) {
      setHint(out, '输入时间戳后自动转换，支持负时间戳（1970 年以前）');
      return;
    }
    if (!/^-?\d+$/.test(raw)) {
      setErr(out, '时间戳必须是整数（秒或毫秒）');
      return;
    }
    var num = parseInt(raw, 10);
    var unitSel = $('p-ts-ts-unit').value;
    var auto = unitSel === 'auto';
    // 自动识别：>= 1e12（13 位量级）按毫秒，否则按秒；负数取绝对值判断量级。
    var unit = auto ? (Math.abs(num) >= 1e12 ? 'ms' : 's') : unitSel;
    var ms = unit === 'ms' ? num : num * 1000;
    if (!isFinite(ms) || isNaN(new Date(ms).getTime())) {
      setErr(out, '时间戳超出有效日期范围');
      return;
    }
    var d = new Date(ms);
    var unitDesc = (auto ? '自动识别为' : '按')
      + (unit === 'ms' ? '毫秒（13 位量级）' : '秒（10 位量级）');
    out.className = 'p-ts-out';
    out.innerHTML =
      line('本地时间', 'p-ts-r-local', fmtLocal(ms), true)
      + line('UTC 时间', 'p-ts-r-utc', fmtUtc(ms) + ' UTC', true)
      + line('ISO 8601', 'p-ts-r-iso', d.toISOString(), true)
      + '<div class="p-ts-meta">星期' + WEEKS[d.getDay()]
      + ' · ' + relTime(ms) + ' · ' + unitDesc + '</div>';
  }

  // ===== 日期解析 =====
  // 优先按本地时区解析常见无歧义格式（避免把 "2026-09-13" 当成 UTC 零点），
  // 再回退浏览器原生解析（支持带 Z / 时区偏移的 ISO 8601 与 RFC 日期）。
  function parseDateText(str) {
    str = str.trim();
    if (!str) return NaN;
    var m = str.replace(/\//g, '-').match(
      /^(\d{4})-(\d{1,2})-(\d{1,2})(?:[ T](\d{1,2}):(\d{1,2})(?::(\d{1,2}))?)?$/
    );
    if (m) {
      var d = new Date(
        +m[1], +m[2] - 1, +m[3],
        +(m[4] || 0), +(m[5] || 0), +(m[6] || 0)
      );
      var t = d.getTime();
      return isNaN(t) ? NaN : t;
    }
    var native = Date.parse(str);
    return isNaN(native) ? NaN : native;
  }

  // ===== 日期 → 时间戳 =====
  function renderDate() {
    var out = $('p-ts-dt-out');
    var text = $('p-ts-dt-text').value.trim();
    var raw, ms;
    if (text) {
      raw = text;
      ms = parseDateText(text);
    } else {
      raw = $('p-ts-dt-picker').value;
      ms = parseDateText(raw);
    }
    if (!raw) {
      setHint(out, '选择日期或直接输入日期字符串后自动转换');
      return;
    }
    if (isNaN(ms)) {
      setErr(out, '无法识别的日期格式，示例：2026-09-13 12:00:00');
      return;
    }
    var sec = Math.floor(ms / 1000);
    var d = new Date(ms);
    out.className = 'p-ts-out';
    out.innerHTML =
      line('秒级时间戳', 'p-ts-r-sec', String(sec), true)
      + line('毫秒时间戳', 'p-ts-r-ms', String(ms), true)
      + line('对应本地', 'p-ts-r-dlocal',
        fmtLocal(ms) + '  星期' + WEEKS[d.getDay()], false)
      + line('对应 UTC', 'p-ts-r-dutc', fmtUtc(ms) + ' UTC', true);
  }

  // ===== 复制（navigator.clipboard 在非安全上下文可能不可用，做兜底）=====
  function legacyCopy(text) {
    var ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    var ok = false;
    try { ok = document.execCommand('copy'); } catch (e) { ok = false; }
    document.body.removeChild(ta);
    return ok;
  }
  function flashBtn(btn, msg) {
    if (!btn.dataset.label) btn.dataset.label = btn.textContent;
    btn.textContent = msg;
    clearTimeout(btn._timer);
    btn._timer = setTimeout(function () {
      btn.textContent = btn.dataset.label;
    }, 1200);
  }
  function copyText(text, btn) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(
        function () { flashBtn(btn, '已复制'); },
        function () {
          if (legacyCopy(text)) flashBtn(btn, '已复制');
          else flashBtn(btn, '复制失败');
        }
      );
    } else if (legacyCopy(text)) {
      flashBtn(btn, '已复制');
    } else {
      flashBtn(btn, '复制失败');
    }
  }

  // ===== 当前时间戳实时刷新 =====
  function tickNow() {
    var d = new Date();
    var ms = d.getTime();
    $('p-ts-now-sec').textContent = Math.floor(ms / 1000);
    $('p-ts-now-ms').textContent = '.' + pad3(ms % 1000);
    $('p-ts-now-msfull').textContent = String(ms);
    $('p-ts-now-date').textContent =
      fmtLocal(ms) + '  星期' + WEEKS[d.getDay()] + '  ·  ' + tzOffsetLabel(d);
  }

  function nowPickerValue(d) {
    return d.getFullYear() + '-' + pad(d.getMonth() + 1) + '-' + pad(d.getDate())
      + 'T' + pad(d.getHours()) + ':' + pad(d.getMinutes()) + ':' + pad(d.getSeconds());
  }

  // ===== 事件绑定 =====
  $('p-ts-ts-input').addEventListener('input', renderTs);
  $('p-ts-ts-unit').addEventListener('change', renderTs);
  $('p-ts-ts-now').addEventListener('click', function () {
    $('p-ts-ts-input').value = Math.floor(Date.now() / 1000);
    $('p-ts-ts-unit').value = 'auto';
    renderTs();
  });

  // 用选择器时清掉文本框，保证“所见即所解析”（文本框非空时优先）。
  $('p-ts-dt-picker').addEventListener('input', function () {
    $('p-ts-dt-text').value = '';
    renderDate();
  });
  $('p-ts-dt-text').addEventListener('input', renderDate);
  $('p-ts-dt-now').addEventListener('click', function () {
    var nowStr = fmtLocal(Date.now());
    $('p-ts-dt-picker').value = nowPickerValue(new Date());
    $('p-ts-dt-text').value = nowStr;
    renderDate();
  });

  // 复制按钮事件委托（结果行是动态渲染的）。
  $('p-ts-wrap').addEventListener('click', function (e) {
    var btn = e.target.closest ? e.target.closest('.p-ts-copy') : null;
    if (!btn) return;
    var node = $(btn.getAttribute('data-copy-id'));
    var val = node ? node.textContent.trim() : '';
    if (val) copyText(val, btn);
  });

  // ===== 初始状态 =====
  $('p-ts-dt-picker').value = nowPickerValue(new Date());
  tickNow();
  setInterval(tickNow, 100);
  renderTs();
  renderDate();
})();

// 数字时钟外部插件：纯前端工具，无需 Rust 端处理。
// 演示外部插件只需 plugin.json + body.html + main.js 三个文件，
// 放入 webview_plugins/ 目录后重启应用即自动出现在「工具插件」分组。
(function () {
  var WEEKS = ['日', '一', '二', '三', '四', '五', '六'];
  function pad(n) { return n < 10 ? '0' + n : '' + n; }
  function tick() {
    var now = new Date();
    document.getElementById('p-clock-date').textContent =
      now.getFullYear() + '-' + pad(now.getMonth() + 1) + '-' + pad(now.getDate());
    document.getElementById('p-clock-week').textContent = '星期' + WEEKS[now.getDay()];
    document.getElementById('p-clock-time').textContent =
      pad(now.getHours()) + ':' + pad(now.getMinutes()) + ':' + pad(now.getSeconds());
  }
  tick();
  setInterval(tick, 1000);
})();

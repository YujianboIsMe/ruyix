/**
 * Darkhorse Code — 多语言模块
 * 中/英双语，配置文件: lang/zh-CN.json, lang/en.json
 */

window.I18N = (() => {
  const LANG_KEY = "ruyix.code.ui.lang";

  let _data = {};
  let _lang = "zh-CN";

  async function init() {
    const invoke = getTauriInvoke();

    let lang = "zh-CN";
    if (invoke) {
      try {
        const v = await invoke("config_get", { scope: "global", key: LANG_KEY, projectRoot: undefined });
        if (v === "en") lang = "en";
      } catch {}
    }
    _lang = lang;

    try {
      const base = window.location.origin || "https://ruyix.localhost";
      const resp = await fetch(`${base}/lang/${lang}.json`);
      if (resp.ok) {
        _data = await resp.json();
      }
    } catch {
      // 降级：空字典，t() 返回 key 本身
    }
  }

  function getLang() {
    return _lang;
  }

  async function setLang(lang) {
    const invoke = getTauriInvoke();
    if (invoke) {
      try {
        await invoke("config_set", { scope: "global", key: LANG_KEY, value: lang, projectRoot: undefined });
      } catch {}
    }
    await init();
  }

  function t(key, params) {
    let s = _data[key] ?? key;

    // 参数替换: {name} → value
    if (params) {
      for (const [k, v] of Object.entries(params)) {
        s = s.replaceAll(`{${k}}`, String(v ?? "?"));
      }
    }

    return s;
  }

  return { init, setLang, getLang, t };
})();

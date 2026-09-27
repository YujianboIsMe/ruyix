/**
 * ruyix — 菜单栏（语言 / 配置 / 能力 / 帮助 / 服务 五个下拉）
 *
 * 这个模块管：`.menu-item` 下拉的统一 toggle、语言切换与选中标记、项目菜单的随状态启用、
 * 配置表单标签的开关、能力四子面板的切换，以及「帮助」「服务」两个菜单的入口。
 *
 * 经典脚本（非 ES module）：与 main.js 共享同一个全局作用域，顶层 function 声明仍是
 * 全局的（initApp 按原次序裸名调用）；应用状态一律读 window.state（main.js 里显式导出）。
 * 门禁：ui-smoke U1/U22/U23/U56。
 */


// ============================================
// 语言菜单
// ============================================

/**
 * 菜单栏统一设置：所有带下拉菜单的 .menu-item 统一处理 toggle 行为
 */
function setupMenuBar() {
  const menuItems = document.querySelectorAll(".menu-item");
  menuItems.forEach((menu) => {
    const dropdown = menu.querySelector(".menu-dropdown");
    if (!dropdown) return;

    menu.addEventListener("click", (e) => {
      e.stopPropagation();
      // 点击下拉项时：不切换本下拉（子项自行处理关闭），只关闭其他下拉
      const onItem = !!e.target.closest(".menu-dropdown-item");
      document.querySelectorAll(".menu-dropdown").forEach((d) => {
        if (d !== dropdown) d.style.display = "none";
      });
      if (!onItem) {
        dropdown.style.display = dropdown.style.display === "none" ? "" : "none";
      }
    });
  });

  // 点击菜单外关闭所有下拉
  document.addEventListener("click", () => {
    document.querySelectorAll(".menu-dropdown").forEach((d) => {
      d.style.display = "none";
    });
  });

  // 语言切换
  const langDropdown = document.getElementById("menu-lang-dropdown");
  if (langDropdown) {
    langDropdown.querySelectorAll("[data-lang]").forEach((item) => {
      item.addEventListener("click", async () => {
        await I18N.setLang(item.dataset.lang);
        langDropdown.style.display = "none";
        refreshI18nUI();
        updateLangMenu();
      });
    });
    // 初始状态：当前语言项显示 ✔️
    updateLangMenu();
  }

  // 项目菜单项点击：统一走命令系统
  const projectDropdown = document.getElementById("menu-project-dropdown");
  if (projectDropdown) {
    projectDropdown.querySelectorAll("[data-action]").forEach((item) => {
      item.addEventListener("click", async () => {
        projectDropdown.style.display = "none";
        const action = item.dataset.action;
        if (action === "close-project") {
          await handleCommand("close project");
        } else if (action === "new-project") {
          const path = await pickFolder(I18N.t("project.pick_folder"));
          if (path) await handleCommand("open project " + path);
        }
      });
    });
  }

  updateProjectMenu();
}

/**
 * 配置菜单：全局 / 项目 / 运行 → 中央编辑区打开配置表单标签
 * （表单本体在 config.js：扫描配置项 → 表单 + 保存/应用/取消）
 */
function setupConfigMenu() {
  const dropdown = document.getElementById("menu-config-dropdown");
  if (!dropdown) return;
  dropdown.querySelectorAll("[data-config-scope]").forEach((item) => {
    item.addEventListener("click", async () => {
      dropdown.style.display = "none";
      await handleCommand("config form " + item.dataset.configScope);
    });
  });
}

/** 打开（或复用）配置表单标签 */
async function openConfigTab(scope) {
  return window.ConfigUI?.open(scope);
}

/** 保存配置表单（Ctrl+S）；应用/取消走各自按钮 */
async function saveConfigTab() {
  return window.ConfigUI?.save();
}

/**
 * 语言菜单选中标记：当前语言项后面显示 ✔️
 */
function updateLangMenu() {
  const current = I18N.getLang();
  document.querySelectorAll("#menu-lang-dropdown [data-lang]").forEach((item) => {
    const check = item.querySelector(".lang-check");
    if (check) check.style.display = item.dataset.lang === current ? "" : "none";
  });
}

/**
 * 根据项目打开状态切换"项目"菜单的子项显示
 */
function updateProjectMenu() {
  const newItem = document.querySelector('[data-action="new-project"]');
  const closeItem = document.querySelector('[data-action="close-project"]');
  if (!newItem || !closeItem) return;

  if (window.state.currentProject) {
    newItem.style.display = "none";
    closeItem.style.display = "";
  } else {
    newItem.style.display = "";
    closeItem.style.display = "none";
  }
}

// ============================================
// 服务页（agent 用 background 起的托管进程；面板读引擎同一份表）
// ============================================
function setupServiceMenu() {
  const btn = document.getElementById("menu-service");
  if (!btn) return;

  btn.addEventListener("click", () => window.ServiceUI?.open());
  btn.style.cursor = "pointer";
}

// ============================================
// 帮助页（正文源文件 = ui/help-zh.md / help-en.md，由 markdown-it 渲染）
// ============================================
function setupHelpMenu() {
  const btn = document.getElementById("menu-help");
  if (!btn) return;

  btn.addEventListener("click", () => openHelp());
  btn.style.cursor = "pointer";

  // 帮助页返回按钮
  document.getElementById("btn-help-back")?.addEventListener("click", () => hideHelpPage());
}

// ============================================
// 导航区标签页切换
// ============================================

function setupCapabilityMenu() {
  // 面板内子标签（MCP / A2A / 工具 / 技能）
  document.querySelectorAll(".cap-sub-tab").forEach((sub) => {
    sub.addEventListener("click", () => switchCapabilitySub(sub.dataset.capTab));
  });
  // 顶部「能力」菜单：激活能力面板并切到对应子页
  document.querySelectorAll("#menu-capability-dropdown [data-cap]").forEach((item) => {
    item.addEventListener("click", () => {
      document.querySelector('.nav-tab[data-tab="capability"]')?.click();
      switchCapabilitySub(item.dataset.cap);
    });
  });
}

function switchCapabilitySub(sub) {
  document.querySelectorAll(".cap-sub-tab").forEach((s) =>
    s.classList.toggle("active", s.dataset.capTab === sub));
  document.querySelectorAll(".cap-sub-panel").forEach((p) =>
    p.classList.toggle("active", p.id === `cap-sub-${sub}`));
}
// ============================================
// 模块导出（经典脚本：这些函数本身也仍是全局的，见文件头）
// ============================================

window.MenusUI = {
  setupMenuBar,
  setupConfigMenu,
  setupCapabilityMenu,
  setupHelpMenu,
  setupServiceMenu,
  updateLangMenu,
  updateProjectMenu,
  openConfigTab,
  saveConfigTab,
  switchCapabilitySub,
};

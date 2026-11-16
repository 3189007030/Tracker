"use strict";

(() => {
    try {
        const theme = localStorage.getItem("theme");
        if (theme === "light" || theme === "dark") {
            document.documentElement.setAttribute("data-theme", theme);
        } else {
            const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
            document.documentElement.setAttribute("data-theme", prefersDark ? "dark" : "light");
        }
        if (window.matchMedia) {
            window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", (e) => {
                if (!localStorage.getItem("theme")) {
                    document.documentElement.setAttribute("data-theme", e.matches ? "dark" : "light");
                }
            });
        }
    } catch (_) {}
})();

document.querySelectorAll("[data-copy-button]").forEach((button) => { button.hidden = false; });

const languageSwitcher = document.querySelector("[data-language-switcher]");
if (languageSwitcher) {
    languageSwitcher.querySelectorAll("a[lang]").forEach((link) => {
        const url = new URL(window.location.href);
        url.searchParams.set("lang", link.getAttribute("lang"));
        link.href = `${url.pathname}?${url.searchParams.toString()}${url.hash}`;
    });
}

const themeToggle = document.querySelector("[data-theme-toggle]");
if (themeToggle) {
    themeToggle.addEventListener("click", () => {
        const current = document.documentElement.getAttribute("data-theme") || (window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark");
        const next = current === "dark" ? "light" : "dark";
        document.documentElement.setAttribute("data-theme", next);
        try { localStorage.setItem("theme", next); } catch (_) {}
    });
}


const menuToggle = document.querySelector("[data-menu-toggle]");
const siteNav = document.querySelector("[data-site-nav]");
if (menuToggle && siteNav) {
    const closeMenu = () => {
        siteNav.classList.remove("is-open");
        menuToggle.setAttribute("aria-expanded", "false");
    };
    menuToggle.addEventListener("click", () => {
        const open = siteNav.classList.toggle("is-open");
        menuToggle.setAttribute("aria-expanded", String(open));
    });
    siteNav.addEventListener("click", (event) => {
        if (event.target.closest("a")) closeMenu();
    });
    document.addEventListener("keydown", (event) => {
        if (event.key === "Escape") closeMenu();
    });
}

const path = window.location.pathname;
const activeKey = path === "/" ? "home" : path.startsWith("/trackers") || path.startsWith("/tracker/") ? "trackers" : path.startsWith("/sources") ? "sources" : path.startsWith("/declaration") ? "declaration" : "";
document.querySelectorAll("[data-nav-key]").forEach((item) => {
    if (item.dataset.navKey === activeKey && item.matches("a")) item.setAttribute("aria-current", "page");
});

document.addEventListener("click", async (event) => {
    const button = event.target.closest("[data-copy-button]");
    if (!button) return;
    const row = button.closest("[data-copy-row]");
    const input = row && row.querySelector("input");
    const status = row && row.querySelector("[role='status']");
    if (!input || !status) return;
    try {
        if (!window.isSecureContext || !navigator.clipboard) throw new Error("Clipboard unavailable");
        await navigator.clipboard.writeText(input.value);
        status.textContent = row.dataset.copyOk;
    } catch (_) {
        status.textContent = row.dataset.copyFail;
        try { input.hidden = false; input.focus(); input.select(); } catch (_) { /* Text remains selectable. */ }
    }
});

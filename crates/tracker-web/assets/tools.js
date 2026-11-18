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

window.trackerTime = (() => {
    const locale = document.documentElement.lang;
    const clock = { hour: "2-digit", minute: "2-digit", hourCycle: "h23" };
    const date = { year: "numeric", month: "2-digit", day: "2-digit" };
    let formats;
    try {
        const minute = { ...date, ...clock, timeZoneName: "shortOffset" };
        formats = {
            minute: new Intl.DateTimeFormat(locale, minute),
            second: new Intl.DateTimeFormat(locale, { ...minute, second: "2-digit" }),
            tick: new Intl.DateTimeFormat(locale, { ...clock, month: "2-digit", day: "2-digit" }),
        };
    } catch (_) {
        // Keep the labelled UTC values if date localization is unavailable.
    }

    const format = (seconds, precision = "minute") => {
        if (!formats || !Number.isFinite(seconds)) return "";
        const date = new Date(seconds * 1000);
        return Number.isFinite(date.getTime()) ? formats[precision].format(date) : "";
    };
    const update = (root) => {
        if (!formats) return;
        root.querySelectorAll("time[datetime]").forEach((element) => {
            const label = format(Date.parse(element.dateTime) / 1000);
            if (label) element.textContent = label;
        });
        root.querySelectorAll("[data-title-timestamp]").forEach((element) => {
            const label = format(Number(element.dataset.titleTimestamp));
            if (label) element.title = element.title.replace(element.dataset.titleUtc, label);
        });
        root.querySelectorAll("[data-local-time-label]").forEach((element) => {
            element.textContent = element.dataset.localTimeLabel;
        });
    };
    update(document);
    return { format, update };
})();

const languageSwitcher = document.querySelector("[data-language-switcher]");
if (languageSwitcher) {
    languageSwitcher.querySelectorAll("a[lang]").forEach((link) => {
        const updateLink = () => {
            const url = new URL(window.location.href);
            url.searchParams.set("lang", link.getAttribute("lang"));
            link.href = `${url.pathname}?${url.searchParams.toString()}${url.hash}`;
        };
        updateLink();
        link.addEventListener("click", updateLink);
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
    menuToggle.hidden = false;
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
        if (event.key === "Escape" && menuToggle.getAttribute("aria-expanded") === "true") {
            closeMenu();
            menuToggle.focus();
        }
    });
}

// Dynamic scroll depth indicator on floating masthead
const masthead = document.querySelector(".masthead");
if (masthead) {
    let ticking = false;
    const updateScrollProgress = () => {
        const doc = document.documentElement;
        const total = doc.scrollHeight - window.innerHeight;
        const progress = total > 0 ? Math.min(1, Math.max(0, window.scrollY / total)) : 0;
        masthead.style.setProperty("--scroll-depth", `${(progress * 100).toFixed(1)}%`);
        masthead.classList.toggle("is-scrolled", window.scrollY > 12);
        ticking = false;
    };
    window.addEventListener("scroll", () => {
        if (!ticking) {
            window.requestAnimationFrame(updateScrollProgress);
            ticking = true;
        }
    }, { passive: true });
    window.addEventListener("resize", updateScrollProgress, { passive: true });
    updateScrollProgress();
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

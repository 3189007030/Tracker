"use strict";

// Progressive enhancement for the tracker list: typing in the search box and
// changing filters/pagination fetches the server-rendered results fragment and
// swaps only that region. Without JavaScript the same GET form and pagination
// links keep working, so the page is fully usable without this script.
(() => {
    const form = document.querySelector("[data-tracker-filters]");
    const results = document.querySelector("[data-tracker-results]");
    if (!form || !results || !window.fetch || !window.URLSearchParams) return;

    form.classList.add("has-live-search");

    const zh = document.documentElement.lang !== "en";
    const text = (cn, en) => (zh ? cn : en);
    const input = form.querySelector("input[name='q']");
    const clearBtn = form.querySelector("[data-search-clear]");
    const protocol = form.querySelector("select[name='protocol']");
    const limit = form.querySelector("select[name='limit']");
    const status = form.querySelector(".filter-status");
    let controller = null;
    let timer = 0;
    let requested = 0;

    const formParams = () => {
        const params = new URLSearchParams();
        new FormData(form).forEach((value, key) => {
            if (key !== "page") params.set(key, String(value));
        });
        return params;
    };
    const revealCopyButtons = () => {
        results.querySelectorAll("[data-copy-button]").forEach((button) => {
            button.hidden = false;
        });
    };
    const updateClearBtn = () => {
        if (clearBtn) {
            clearBtn.hidden = !input || !input.value;
        }
    };
    const syncForm = (params) => {
        if (input) input.value = params.get("q") || "";
        if (protocol) protocol.value = params.get("protocol") || "";
        if (limit && params.get("limit")) limit.value = params.get("limit");
        updateClearBtn();
    };
    const showStatus = (message) => {
        if (status) status.textContent = message;
    };
    const cancelPending = () => {
        clearTimeout(timer);
        timer = 0;
        if (controller) controller.abort();
        requested += 1;
    };

    const load = async (page, mode) => {
        const params = formParams();
        if (page > 1) params.set("page", String(page));
        const ticket = ++requested;
        if (controller) controller.abort();
        controller = new AbortController();
        results.setAttribute("aria-busy", "true");
        showStatus(text("正在更新…", "Updating…"));
        try {
            const response = await fetch(`/trackers/partial?${params.toString()}`, {
                headers: { Accept: "text/html" },
                signal: controller.signal
            });
            if (!response.ok) throw new Error(String(response.status));
            const fragment = await response.text();
            if (ticket !== requested) return;
            results.innerHTML = fragment;
            window.trackerTime?.update(results);
            revealCopyButtons();
            const query = params.toString();
            const url = `/trackers${query ? `?${query}` : ""}`;
            if (mode === "push") history.pushState({ trackers: true }, "", url);
            else if (mode === "replace") history.replaceState({ trackers: true }, "", url);
            showStatus("");
            if (page > 1 || mode === "push") {
                const rect = results.getBoundingClientRect();
                if (rect.top < 0) {
                    const reduceMotion = typeof window.matchMedia === "function"
                        && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
                    window.scrollTo({
                        top: window.scrollY + rect.top - 24,
                        behavior: reduceMotion ? "auto" : "smooth"
                    });
                }
            }
        } catch (error) {
            if (error && error.name === "AbortError") return;
            showStatus(text("更新失败，可提交表单重试。", "Update failed; submit the form to retry."));
        } finally {
            if (ticket === requested) results.setAttribute("aria-busy", "false");
        }
    };
    const schedule = () => {
        clearTimeout(timer);
        timer = setTimeout(() => {
            timer = 0;
            load(1, "replace");
        }, 300);
    };
    const clearSearch = () => {
        cancelPending();
        if (input) {
            input.value = "";
            input.focus();
        }
        updateClearBtn();
        load(1, "replace");
    };

    if (input) {
        input.addEventListener("input", () => {
            updateClearBtn();
            schedule();
        });
        input.addEventListener("keydown", (event) => {
            if (event.key !== "Escape" || event.repeat) return;
            const updatePending = timer !== 0 || results.getAttribute("aria-busy") === "true";
            if (!input.value && !updatePending) return;
            event.preventDefault();
            clearSearch();
        });
    }
    if (clearBtn) {
        clearBtn.addEventListener("click", clearSearch);
    }
    if (protocol) protocol.addEventListener("change", () => { cancelPending(); load(1, "replace"); });
    if (limit) limit.addEventListener("change", () => { cancelPending(); load(1, "replace"); });
    form.addEventListener("submit", (event) => {
        event.preventDefault();
        cancelPending();
        load(1, "push");
    });
    results.addEventListener("click", (event) => {
        const link = event.target.closest("a[data-page]");
        if (!link) return;
        event.preventDefault();
        const page = Number(link.dataset.page);
        if (Number.isInteger(page) && page > 0) {
            cancelPending();
            load(page, "push");
        }
    });
    results.addEventListener("submit", (event) => {
        const jump = event.target.closest("[data-page-jump]");
        if (!jump) return;
        event.preventDefault();
        const field = jump.querySelector("input[name='page']");
        const page = Number(field && field.value);
        if (Number.isInteger(page) && page > 0) {
            cancelPending();
            load(page, "push");
        }
    });
    window.addEventListener("popstate", () => {
        cancelPending();
        const params = new URLSearchParams(location.search);
        syncForm(params);
        const page = Number(params.get("page") || "1");
        load(Number.isInteger(page) && page > 0 ? page : 1, "none");
    });

    updateClearBtn();
    revealCopyButtons();
})();

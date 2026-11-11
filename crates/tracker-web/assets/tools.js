"use strict";

document.querySelectorAll("[data-copy-button]").forEach((button) => { button.hidden = false; });
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

// =============================================================================
// Global utility functions
// =============================================================================

function copyIdToClipboard(event) {
  const target = event.target;
  const id = target.getAttribute("data-value");
  navigator.clipboard.writeText(id);
  target.classList.add("-active");

  setTimeout(() => {
    target.classList.remove("-active");
  }, 1000);
}

function previewAvatar(event) {
  document.querySelector(".m-profileSummary__userImage").src =
    URL.createObjectURL(event.target.files[0]);
}

function togglePersonaList() {
  const selectedOption = document.querySelector("#follow-type-select").value;
  const personaList = document.querySelector(
    ".o-followDialog__personaList",
  );

  if (
    selectedOption === "follow_all" ||
    selectedOption === "follow_only"
  ) {
    personaList.classList.add("-visible");
  } else {
    personaList.classList.remove("-visible");
  }
}

window.toggleEmojiPicker = function (pickerId, event) {
  event.preventDefault();
  event.stopPropagation();
  const picker =
    document.getElementById(pickerId) ||
    document.querySelector(pickerId);
  if (!picker) return;

  const wasHidden = picker.classList.contains("-hidden");
  picker.classList.toggle("-hidden");

  if (wasHidden) {
    const ep = picker.querySelector("emoji-picker");
    if (ep && ep.shadowRoot) {
      const searchInput = ep.shadowRoot.querySelector("#search");
      if (searchInput) searchInput.focus();
    }
    // Close on click outside
    const closeOnClickOutside = (e) => {
      if (!picker.contains(e.target)) {
        picker.classList.add("-hidden");
        document.removeEventListener("click", closeOnClickOutside);
      }
    };
    setTimeout(
      () => document.addEventListener("click", closeOnClickOutside),
      0,
    );
  }
};

window.insertMediaSyntax = function (eventId, targetSelector) {
  // Fall back to the target stored on the media list dialog (set when it was opened)
  if (!targetSelector) {
    const mediaList = document.getElementById("media-list");
    targetSelector = mediaList?.dataset.target;
  }
  const textarea = document.querySelector(targetSelector);
  const syntax = "![media](rostra-media:" + eventId + ")";

  if (textarea) {
    const start = textarea.selectionStart;
    const end = textarea.selectionEnd;
    const text = textarea.value;

    const newText = text.substring(0, start) + syntax + text.substring(end);
    textarea.value = newText;

    const newPos = start + syntax.length;
    textarea.setSelectionRange(newPos, newPos);
    textarea.focus();

    textarea.dispatchEvent(new Event("input", { bubbles: true }));
  }

  const mediaList = document.querySelector(".o-mediaList");
  if (mediaList) {
    mediaList.style.display = "none";
  }
};

window.uploadMediaFile = function (inputEl) {
  const file = inputEl.files[0];
  if (!file) return;

  const dialog = document.getElementById("media-list");
  const previewEl = document.getElementById("media-upload-preview");
  if (!previewEl || !dialog) return;

  // Show client-side preview before uploading
  const blobUrl = URL.createObjectURL(file);
  const mediaEl = previewEl.querySelector(".o-mediaList__previewMedia");
  const nameEl = previewEl.querySelector(".o-mediaList__previewName");

  mediaEl.innerHTML = "";
  if (file.type.startsWith("image/")) {
    const img = document.createElement("img");
    img.src = blobUrl;
    mediaEl.appendChild(img);
  } else if (file.type.startsWith("video/")) {
    const video = document.createElement("video");
    video.src = blobUrl;
    video.controls = true;
    video.muted = true;
    mediaEl.appendChild(video);
  } else if (file.type.startsWith("audio/")) {
    const audio = document.createElement("audio");
    audio.src = blobUrl;
    audio.controls = true;
    mediaEl.appendChild(audio);
  }
  nameEl.textContent = file.name;

  // Switch dialog to preview mode: hide items, swap title & buttons
  const titleEl = dialog.querySelector(".o-mediaList__title");
  const itemsEl = dialog.querySelector(".o-mediaList__items");
  const uploadBtn = dialog.querySelector(".o-mediaList__uploadButton");
  const closeBtn = dialog.querySelector(".o-mediaList__closeButton");

  const origTitle = titleEl.textContent;
  const origUploadClick = uploadBtn.onclick;
  const origCloseClick = closeBtn.onclick;

  titleEl.textContent = "Preview media to upload";
  if (itemsEl) itemsEl.style.display = "none";
  previewEl.classList.add("-active");
  closeBtn.lastChild.textContent = "Cancel";

  const cleanup = () => {
    previewEl.classList.remove("-active");
    if (itemsEl) itemsEl.style.display = "";
    titleEl.textContent = origTitle;
    closeBtn.lastChild.textContent = "Close";
    uploadBtn.onclick = origUploadClick;
    closeBtn.onclick = origCloseClick;
    URL.revokeObjectURL(blobUrl);
  };

  uploadBtn.onclick = () => {
    cleanup();
    doMediaUpload(inputEl, file);
  };
  closeBtn.onclick = () => {
    cleanup();
    inputEl.value = "";
  };
};

function doMediaUpload(inputEl, file) {
  const formData = new FormData();
  formData.append("media_file", file);

  const progressEl = document.getElementById("upload-progress");
  const fillEl = document.getElementById("upload-progress-fill");
  const textEl = document.getElementById("upload-progress-text");

  if (progressEl) progressEl.classList.add("-active");
  if (fillEl) fillEl.style.width = "0%";
  if (textEl) textEl.textContent = "0%";

  const xhr = new XMLHttpRequest();

  xhr.upload.addEventListener("progress", (e) => {
    if (e.lengthComputable) {
      const percent = Math.round((e.loaded / e.total) * 100);
      if (fillEl) fillEl.style.width = percent + "%";
      if (textEl) textEl.textContent = percent + "%";
    }
  });

  xhr.addEventListener("load", () => {
    if (progressEl) progressEl.classList.remove("-active");
    inputEl.value = "";

    if (xhr.status >= 200 && xhr.status < 300) {
      // Parse response and execute scripts (same as alpine-ajax would)
      const tpl = document.createElement("template");
      tpl.innerHTML = xhr.responseText;
      const scripts = tpl.content.querySelectorAll("script");
      scripts.forEach((script) => {
        const s = document.createElement("script");
        s.textContent = script.textContent;
        document.body.appendChild(s);
        s.remove();
      });
    } else {
      let message = "Upload failed (" + xhr.status + ")";
      try {
        const body = JSON.parse(xhr.responseText);
        if (body.message) message = body.message;
      } catch (_) {}
      window.dispatchEvent(
        new CustomEvent("notify", {
          detail: { type: "error", message },
        }),
      );
    }
  });

  xhr.addEventListener("error", () => {
    if (progressEl) progressEl.classList.remove("-active");
    inputEl.value = "";
    window.dispatchEvent(
      new CustomEvent("notify", {
        detail: { type: "error", message: "Upload failed - network error" },
      }),
    );
  });

  xhr.open("POST", "/media/publish");
  xhr.setRequestHeader("X-Alpine-Request", "true");
  xhr.send(formData);
}

// =============================================================================
// Persona Tag Multi-Select Combobox
// =============================================================================

function initPersonaTagSelects() {
  document
    .querySelectorAll(".m-personaTagSelect:not(.-initialized)")
    .forEach((el) => {
      el.classList.add("-initialized");
      updatePersonaTagSelectLabel(el);
      // Focus the toggle if it has tabindex (keyboard-navigable)
      const toggle = el.querySelector(".m-personaTagSelect__toggle[tabindex]");
      if (toggle) toggle.focus();
    });
}

function personaTagSelectToggle(toggleEl) {
  const widget = toggleEl.closest(".m-personaTagSelect");
  const wasOpen = widget.classList.contains("-open");
  widget.classList.toggle("-open");

  // Position the fixed dropdown relative to the toggle button
  if (!wasOpen) {
    const dropdown = widget.querySelector(".m-personaTagSelect__dropdown");
    const rect = toggleEl.getBoundingClientRect();
    dropdown.style.top = rect.bottom + "px";
    dropdown.style.left = rect.left + "px";
    dropdown.style.width = Math.max(rect.width, 160) + "px";
  }
}

function updatePersonaTagSelectLabel(widget) {
  const checked = [
    ...widget.querySelectorAll("input[type=checkbox]:checked"),
  ].map((cb) => cb.value);
  const label = widget.querySelector(".m-personaTagSelect__toggleLabel");
  const emptyLabel = widget.dataset.emptyLabel || "Select tags";
  label.textContent =
    checked.length > 0 ? checked.join(", ") : emptyLabel;
}

function personaTagSelectChanged(checkbox) {
  const widget = checkbox.closest(".m-personaTagSelect");
  updatePersonaTagSelectLabel(widget);
}

// Arrow/j/k cycles persona tag selection when preview dialog is active
document.addEventListener("keydown", (e) => {
  const isDown = e.key === "ArrowDown" || e.key === "j";
  const isUp = e.key === "ArrowUp" || e.key === "k";
  if (!isDown && !isUp) return;

  const dialog = document.querySelector(".o-previewDialog.-active");
  if (!dialog) return;

  const widget = dialog.querySelector(".m-personaTagSelect");
  if (!widget) return;

  const checkboxes = [...widget.querySelectorAll("input[type=checkbox]")];
  if (checkboxes.length === 0) return;

  e.preventDefault();

  const checkedIdx = checkboxes.findIndex((cb) => cb.checked);
  const delta = isDown ? 1 : -1;

  let nextIdx;
  if (checkedIdx < 0) {
    nextIdx = isDown ? 0 : checkboxes.length - 1;
  } else {
    nextIdx = (checkedIdx + delta + checkboxes.length) % checkboxes.length;
  }

  checkboxes.forEach((cb) => (cb.checked = false));
  checkboxes[nextIdx].checked = true;
  updatePersonaTagSelectLabel(widget);
});

function personaTagSelectAddFromInput(input) {
  const tag = input.value.trim().toLowerCase();
  if (!tag || tag.length > 32) return;

  const widget = input.closest(".m-personaTagSelect");

  // Check if tag already exists
  const existing = widget.querySelector(
    `input[type=checkbox][value="${CSS.escape(tag)}"]`,
  );
  if (existing) {
    existing.checked = true;
  } else {
    const name =
      widget.querySelector("input[type=checkbox]")?.name || "persona_tags";
    const options = widget.querySelector(".m-personaTagSelect__options");
    const label = document.createElement("label");
    label.className = "m-personaTagSelect__option";
    label.innerHTML =
      `<input type="checkbox" name="${name}" value="${tag}" checked onchange="personaTagSelectChanged(this)">` +
      `<span>${tag}</span>`;
    options.appendChild(label);
  }
  input.value = "";
  updatePersonaTagSelectLabel(widget);
}

// Close persona tag selects on outside click
document.addEventListener("click", (e) => {
  document.querySelectorAll(".m-personaTagSelect.-open").forEach((el) => {
    if (!el.contains(e.target)) el.classList.remove("-open");
  });
});

// Enter in add-input: add the tag, prevent form submit
document.addEventListener("keydown", (e) => {
  if (
    e.key === "Enter" &&
    e.target.matches(".m-personaTagSelect__addInput")
  ) {
    e.preventDefault();
    personaTagSelectAddFromInput(e.target);
  }
});

// Blur on add-input: add the tag if non-empty
document.addEventListener(
  "focusout",
  (e) => {
    if (e.target.matches(".m-personaTagSelect__addInput")) {
      personaTagSelectAddFromInput(e.target);
    }
  },
  true,
);

// =============================================================================
// Keyboard shortcuts help dialog
// =============================================================================

(function () {
  const shortcuts = [
    ["j / \u2193", "Next post"],
    ["k / \u2191", "Previous post"],
    ["l / \u2192", "Expand replies"],
    ["Enter", "Open post"],
    ["r", "Reply to post"],
    ["h / \u2190", "Go back"],
    ["n", "New post"],
    ["1-5", "Switch tab"],
    ["q", "Home"],
    ["Ctrl+Enter", "Preview / submit"],
    ["Ctrl+Shift+Enter", "Publish from preview"],
    ["Esc", "Close / unfocus / cancel"],
    ["?", "Show this help"],
  ];

  let dialog = null;

  function createDialog() {
    dialog = document.createElement("div");
    dialog.className = "o-shortcutsDialog";

    const rows = shortcuts
      .map(
        ([key, desc]) =>
          `<div class="o-shortcutsDialog__row">` +
          `<kbd class="o-shortcutsDialog__key">${key}</kbd>` +
          `<span class="o-shortcutsDialog__desc">${desc}</span>` +
          `</div>`,
      )
      .join("");

    dialog.innerHTML =
      `<div class="o-shortcutsDialog__backdrop"></div>` +
      `<div class="o-shortcutsDialog__content" role="dialog" aria-label="Keyboard shortcuts">` +
      `<h4 class="o-shortcutsDialog__title">Keyboard Shortcuts</h4>` +
      rows +
      `</div>`;

    dialog.querySelector(".o-shortcutsDialog__backdrop").addEventListener(
      "click",
      () => {
        dialog.classList.remove("-active");
      },
    );

    document.body.appendChild(dialog);
  }

  function toggle() {
    if (!dialog) createDialog();
    dialog.classList.toggle("-active");
  }

  function isInputFocused() {
    const el = document.activeElement;
    if (!el) return false;
    const tag = el.tagName;
    return (
      tag === "TEXTAREA" ||
      tag === "SELECT" ||
      (tag === "INPUT" && el.type !== "hidden")
    );
  }

  document.addEventListener("keydown", (e) => {
    // Close on Escape regardless of focus
    if (e.key === "Escape" && dialog && dialog.classList.contains("-active")) {
      e.preventDefault();
      dialog.classList.remove("-active");
      return;
    }

    if (e.key === "?" && !isInputFocused()) {
      e.preventDefault();
      toggle();
    }
  });
})();

// =============================================================================
// Escape key: blur new-post textarea, cancel inline reply
// =============================================================================

document.addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;

  const el = document.activeElement;
  if (!el || el.tagName !== "TEXTAREA") return;

  // Inline reply textarea — click the cancel button
  if (el.classList.contains("m-inlineReply__content")) {
    e.preventDefault();
    const reply = el.closest(".m-inlineReply");
    if (reply) {
      const cancelBtn = reply.querySelector(".m-inlineReply__cancelButton");
      if (cancelBtn) cancelBtn.click();
    }
    return;
  }

  // New post textarea — just blur
  if (el.classList.contains("m-newPostForm__content")) {
    e.preventDefault();
    el.blur();
  }
});

// =============================================================================
// Keyboard navigation (j/k/l/r) for timeline posts
// =============================================================================

(function () {
  const NAV_SELECTOR =
    ".m-postContext__postParent, " +
    ".m-postContext__postParent ~ .m-postContext__postView, " +
    ".o-mainBarTimeline__item:not(.-preview):not(:has(.m-postContext__postParent)), " +
    ".o-postOverview__repliesItem";
  const SELECTED_CLASS = "-keyboard-selected";

  let selectedEl = null;

  function getItems() {
    return document.querySelectorAll(NAV_SELECTOR);
  }

  function getItemHref(el) {
    const main = el.querySelector(".m-postView__main[data-href]");
    return main ? main.dataset.href : null;
  }

  function saveSelection() {
    const href = selectedEl ? getItemHref(selectedEl) : null;
    history.replaceState({ ...history.state, selectedHref: href }, "");
  }

  function clearSelection() {
    if (selectedEl) {
      selectedEl.classList.remove(SELECTED_CLASS);
      selectedEl = null;
      saveSelection();
    }
  }

  function selectItem(idx) {
    const items = getItems();
    if (items.length === 0) return;
    if (idx < 0) idx = 0;
    if (idx >= items.length) idx = items.length - 1;

    clearSelection();
    selectedEl = items[idx];
    selectedEl.classList.add(SELECTED_CLASS);
    document.body.classList.add("-keyboard-nav");

    // Auto-expand collapsed parent posts
    if (
      selectedEl.classList.contains("m-postContext__postParent") &&
      !selectedEl.classList.contains("-expanded")
    ) {
      selectedEl.classList.add("-expanded");
    }

    saveSelection();

    // Parent posts: scroll timeline item to top. Everything else: just ensure visible.
    if (selectedEl.classList.contains("m-postContext__postParent")) {
      const scrollTarget =
        selectedEl.closest(".o-mainBarTimeline__item") || selectedEl;
      const tabs = document.querySelector(".o-mainBarTimeline__tabs");
      const offset = tabs ? tabs.getBoundingClientRect().bottom : 0;
      const top =
        scrollTarget.getBoundingClientRect().top +
        document.body.scrollTop -
        offset;
      document.body.scrollTo({ top, behavior: "instant" });
    } else {
      selectedEl.scrollIntoView({ block: "nearest" });
    }
  }

  function selectedIndex() {
    if (!selectedEl) return -1;
    const items = getItems();
    for (let i = 0; i < items.length; i++) {
      if (items[i] === selectedEl) return i;
    }
    // Selected element was removed from DOM
    selectedEl = null;
    return -1;
  }

  function isInView(el) {
    const rect = el.getBoundingClientRect();
    return rect.bottom > 0 && rect.top < window.innerHeight;
  }

  function firstVisibleIndex(items) {
    for (let i = 0; i < items.length; i++) {
      if (isInView(items[i])) return i;
    }
    return 0;
  }

  function lastVisibleIndex(items) {
    for (let i = items.length - 1; 0 <= i; i--) {
      if (isInView(items[i])) return i;
    }
    return items.length - 1;
  }

  function isInputFocused() {
    const el = document.activeElement;
    if (!el) return false;
    const tag = el.tagName;
    return (
      tag === "TEXTAREA" ||
      tag === "SELECT" ||
      (tag === "INPUT" && el.type !== "hidden")
    );
  }

  // Find the nearest button bar for a navigable item.
  function getButtonBar(item) {
    // Parent posts: > .m-postView > .m-postView__body > .m-postView__buttonBar
    // PostView (when split from parent): same path
    if (
      item.classList.contains("m-postContext__postParent") ||
      item.classList.contains("m-postContext__postView")
    ) {
      return item.querySelector(
        ":scope > .m-postView > .m-postView__body > .m-postView__buttonBar",
      );
    }
    // Timeline items and reply items:
    // > .m-postContext > .m-postContext__postView > .m-postView > .m-postView__body > .m-postView__buttonBar
    return item.querySelector(
      ":scope > .m-postContext > .m-postContext__postView > .m-postView > .m-postView__body > .m-postView__buttonBar",
    );
  }

  document.addEventListener("keydown", (e) => {
    if (isInputFocused()) return;
    if (document.querySelector(".o-previewDialog.-active")) return;

    // Page-global shortcuts (work on any page)
    if (e.key >= "1" && e.key <= "9") {
      const tabs = document.querySelector(".o-mainBarTimeline__tabs");
      if (!tabs) return;
      const links = [...tabs.querySelectorAll("a[href]:not(.o-mainBarTimeline__back)")];
      const tabIdx = parseInt(e.key, 10) - 1;
      if (tabIdx < links.length) {
        e.preventDefault();
        links[tabIdx].click();
      }
      return;
    } else if (e.key === "q") {
      e.preventDefault();
      window.location.href = "/";
      return;
    } else if (e.key === "h" || e.key === "ArrowLeft") {
      e.preventDefault();
      history.back();
      return;
    } else if (e.key === "n") {
      const textarea = document.getElementById("new-post-content");
      if (textarea) {
        e.preventDefault();
        textarea.focus();
      }
      return;
    }

    // Timeline post navigation (requires posts on the page)
    const items = getItems();
    if (items.length === 0) return;

    const idx = selectedIndex();

    if (e.key === "j" || e.key === "ArrowDown") {
      e.preventDefault();
      if (idx < 0 || !isInView(selectedEl)) {
        selectItem(firstVisibleIndex(items));
      } else {
        selectItem(idx + 1);
      }
    } else if (e.key === "k" || e.key === "ArrowUp") {
      e.preventDefault();
      if (idx < 0 || !isInView(selectedEl)) {
        selectItem(lastVisibleIndex(items));
      } else {
        selectItem(idx - 1);
      }
    } else if (e.key === "l" || e.key === "ArrowRight") {
      e.preventDefault();
      if (!selectedEl) return;
      const bar = getButtonBar(selectedEl);
      if (!bar) return;
      const btn = bar.querySelector(".m-postView__repliesButton");
      if (btn && !btn.classList.contains("u-hidden")) {
        btn.click();
      }
    } else if (e.key === "Enter") {
      if (!selectedEl) return;
      const main = selectedEl.querySelector(".m-postView__main[data-href]");
      if (main) {
        e.preventDefault();
        window.location = main.dataset.href;
      }
    } else if (e.key === "r") {
      e.preventDefault();
      if (!selectedEl) return;
      const bar = getButtonBar(selectedEl);
      if (!bar) return;
      const btn = bar.querySelector(".m-postView__replyToButton");
      if (btn) {
        btn.click();
      }
    }
  });

  // Clear selection when user clicks outside navigable items or focuses a text field
  document.addEventListener("click", (e) => {
    if (!e.target.closest(NAV_SELECTOR)) {
      clearSelection();
    }
  });

  document.addEventListener(
    "focusin",
    (e) => {
      if (isInputFocused()) {
        clearSelection();
      }
    },
    true,
  );

  document.addEventListener("mousemove", () => {
    document.body.classList.remove("-keyboard-nav");
  });

  // Restore selection from history state (e.g. after pressing Back)
  const savedHref = history.state?.selectedHref;
  if (savedHref) {
    const items = getItems();
    for (let i = 0; i < items.length; i++) {
      if (getItemHref(items[i]) === savedHref) {
        selectItem(i);
        break;
      }
    }
  }
})();

// =============================================================================
// aria-keyshortcuts annotations
// =============================================================================

function applyAriaKeyShortcuts() {
  const mappings = [
    ["#timeline-posts", "j k ArrowDown ArrowUp"],
    ["#new-post-content", "n"],
    [".m-newPostForm__content", "Escape Control+Enter"],
    [".m-inlineReply__content", "Escape Control+Enter"],
    [".m-inlineReply__cancelButton", "Escape"],
    [".m-postView__repliesButton", "l ArrowRight"],
    [".m-postView__replyToButton", "r"],
    [".o-previewDialog__submitButton", "Control+Shift+Enter"],
    [".m-personaTagSelect", "j k ArrowDown ArrowUp"],
    [".o-mainBarTimeline__tabs", "1 2 3 4 5"],
  ];
  for (const [sel, key] of mappings) {
    document.querySelectorAll(sel).forEach((el) => {
      el.setAttribute("aria-keyshortcuts", key);
    });
  }
  document.body.setAttribute("aria-keyshortcuts", "? Escape");
}

// =============================================================================
// DOMContentLoaded handlers
// =============================================================================

document.addEventListener("DOMContentLoaded", () => {
  // Initialize persona tag select widgets
  initPersonaTagSelects();

  // Apply aria-keyshortcuts to interactive elements
  applyAriaKeyShortcuts();

  // Prevent flickering of images when they are already in the cache
  const images = document.querySelectorAll('img[loading="lazy"]');
  images.forEach((img) => {
    const testImg = new Image();
    testImg.src = img.src;
    if (testImg.complete) {
      img.removeAttribute("loading");
    }
  });

  // Close action menus on click outside
  document.addEventListener("click", (e) => {
    document.querySelectorAll(".m-postView__actionMenu[open]").forEach((menu) => {
      if (!menu.contains(e.target)) {
        menu.removeAttribute("open");
      }
    });
  });

  // Trigger Prism.js highlighting
  if (window.Prism) {
    Prism.highlightAll();
  }

  // Play/pause videos based on visibility
  const videoObserver = new IntersectionObserver(
    (entries) => {
      entries.forEach((entry) => {
        const video = entry.target;
        if (entry.isIntersecting) {
          video.play().catch(() => {});
        } else {
          video.pause();
        }
      });
    },
    { threshold: 0.5 },
  );

  // Observe existing videos
  document
    .querySelectorAll(".m-rostraMedia__video")
    .forEach((v) => videoObserver.observe(v));

  // Observe new videos added dynamically (for AJAX/htmx)
  const mutationObserver = new MutationObserver((mutations) => {
    mutations.forEach((mutation) => {
      mutation.addedNodes.forEach((node) => {
        if (node.nodeType === 1) {
          node
            .querySelectorAll?.(".m-rostraMedia__video")
            .forEach((v) => videoObserver.observe(v));
          if (node.classList?.contains("m-rostraMedia__video")) {
            videoObserver.observe(node);
          }
        }
      });
    });
  });
  mutationObserver.observe(document.body, { childList: true, subtree: true });
});

// =============================================================================
// Window event listeners
// =============================================================================

// Re-initialize after AJAX swaps (e.g. preview dialog, expanded replies)
window.addEventListener("ajax:after", () => {
  initPersonaTagSelects();
  applyAriaKeyShortcuts();
});

// Suppress unhandled promise rejections from TypeErrors — these are typically
// transient (e.g. emoji-picker background updates, clicking during page load)
// and not actionable for the user. Actual AJAX errors are handled separately
// by the @ajax:error handler.
window.addEventListener("unhandledrejection", (event) => {
  if (event.reason instanceof TypeError) {
    event.preventDefault();
  }
});

// When alpine-ajax gets a non-2xx response whose body doesn't contain
// the expected target element, it fires ajax:missing and then falls back
// to a native form resubmission (which navigates the browser to the raw
// JSON error page). Preventing default on ajax:missing stops that fallback;
// the ajax:error handler (in the notifications component) has already
// dispatched a toast notification by this point.
window.addEventListener("ajax:missing", (event) => {
  if (!event.detail.response.ok) {
    event.preventDefault();
  }
});

// =============================================================================
// Alpine.js component registrations
// =============================================================================

document.addEventListener("alpine:init", () => {
  // Notifications component for the body element
  Alpine.data("notifications", () => ({
    notifications: [],
    nextId: 1,
    init() {
      // Handle AJAX errors
      window.addEventListener("ajax:error", (event) => {
        const detail = event.detail?.xhr || event.detail;
        const status = detail?.status;
        const raw = event.detail?.raw || detail?.raw;
        let message;

        // Try to extract message from JSON response body
        if (raw) {
          try {
            const body = JSON.parse(raw);
            if (body.message) {
              message = body.message;
            }
          } catch (_) {}
        }

        if (!message) {
          if (status === 0 || status === undefined) {
            message = "\u26a0 Network Error - Unable to complete request";
          } else if (status >= 500) {
            message = "\u26a0 Server Error (" + status + ")";
          } else if (status >= 400) {
            message = "\u26a0 Request Failed (" + status + ")";
          }
        }
        if (message) {
          this.addNotification("error", message);
        }

        // Session expired — navigate to login page after a brief
        // delay so the user can see the toast notification.
        if (status === 401) {
          setTimeout(() => {
            window.location.href = "/unlock";
          }, 1500);
        }
      });
      // Clear error notifications on successful AJAX
      window.addEventListener("ajax:success", () => {
        this.clearErrorNotifications();
      });
      // Handle custom notify events
      window.addEventListener("notify", (event) => {
        this.addNotification(
          event.detail.type || "info",
          event.detail.message,
          event.detail.duration,
        );
      });
    },
    addNotification(type, message, duration = null) {
      // Check if this exact message already exists
      const exists = this.notifications.some(
        (n) => n.message === message && n.type === type,
      );
      if (exists) {
        return; // Don't add duplicate
      }

      const id = this.nextId++;
      this.notifications.push({ id, type, message });

      // Auto-dismiss all notification types
      const dismissTime =
        duration !== null ? duration : type === "error" ? 8000 : 4000;
      setTimeout(() => {
        this.removeNotification(id);
      }, dismissTime);
    },
    removeNotification(id) {
      this.notifications = this.notifications.filter((n) => n.id !== id);
    },
    clearErrorNotifications() {
      this.notifications = this.notifications.filter(
        (n) => n.type !== "error",
      );
    },
  }));

  // Generic WebSocket handler - just handles connection and HTML morphing
  Alpine.data("websocket", (url) => ({
    ws: null,
    init() {
      this.connect(url);
    },
    connect(url) {
      const protocol =
        window.location.protocol === "https:" ? "wss:" : "ws:";
      const wsUrl = `${protocol}//${window.location.host}${url}`;

      this.ws = new WebSocket(wsUrl);

      this.ws.onopen = () => {
        console.log("WebSocket connected");
      };

      this.ws.onmessage = (event) => {
        const tpl = document.createElement("template");
        tpl.innerHTML = event.data;

        const focus = (el) => {
          const target = el?.matches?.("[x-autofocus]")
            ? el
            : el?.querySelector?.("[x-autofocus]");
          if (target) target.scrollIntoView({ block: "nearest" });
        };

        // Process elements with IDs - morph or merge based on x-merge attribute
        tpl.content.querySelectorAll("[id]").forEach((content) => {
          const target = document.getElementById(content.id);
          if (!target) return;

          const merge = content.getAttribute("x-merge");
          if (merge === "append") {
            target.append(...content.childNodes);
            Alpine.initTree(target.lastElementChild);
            focus(target.lastElementChild);
          } else if (merge === "prepend") {
            target.prepend(...content.childNodes);
            Alpine.initTree(target.firstElementChild);
            focus(target.firstElementChild);
          } else {
            Alpine.morph(target, content);
            focus(target);
          }
        });

        // Run standalone x-init elements (for $dispatch etc.)
        tpl.content.querySelectorAll("[x-init]").forEach((el) => {
          document.body.appendChild(el);
          Alpine.initTree(el);
          el.remove();
        });
      };

      this.ws.onerror = (error) => {
        // Suppress error logging - it's expected if WebSocket isn't available
      };

      this.ws.onclose = () => {
        // Don't reconnect - WebSocket is optional
        // If we want reconnection, it should be with exponential backoff
      };
    },
    destroy() {
      if (this.ws) {
        this.ws.close();
      }
    },
  }));

  // Badge counts component - reactive state for tab badges
  Alpine.data("badgeCounts", (initial) => ({
    followees: initial?.followees || 0,
    network: initial?.network || 0,
    notifications: initial?.notifications || 0,
    shoutbox: initial?.shoutbox || 0,
    init() {
      // Set up reactive title updates based on notifications
      this.$watch("notifications", (count) => {
        document.title =
          count > 9
            ? "Rostra (9+)"
            : count > 0
              ? `Rostra (${count})`
              : "Rostra";
      });
      // Set initial title
      document.title =
        this.notifications > 9
          ? "Rostra (9+)"
          : this.notifications > 0
            ? `Rostra (${this.notifications})`
            : "Rostra";
    },
    onUpdate(detail) {
      this.followees = detail.followees || 0;
      this.network = detail.network || 0;
      this.notifications = detail.notifications || 0;
      this.shoutbox = detail.shoutbox || 0;
    },
    formatCount(count) {
      return count > 9 ? " (9+)" : count > 0 ? ` (${count})` : "";
    },
  }));

  // Subsequence fuzzy match: each query char must appear in order in text.
  // Returns score > 0 on match, 0 on no match. Rewards consecutive matches
  // and matches at word boundaries (after _, -, space, or at start).
  function fuzzyMatch(query, text) {
    const q = query.toLowerCase();
    const t = text.toLowerCase();

    // Quick subsequence check
    let qi = 0;
    for (let ti = 0; ti < t.length && qi < q.length; ti++) {
      if (t[ti] === q[qi]) qi++;
    }
    if (qi < q.length) return 0;

    // Score the match
    let score = 0;
    qi = 0;
    let prevMatchIdx = -2;
    for (let ti = 0; ti < t.length && qi < q.length; ti++) {
      if (t[ti] === q[qi]) {
        score += 1;
        if (ti === prevMatchIdx + 1) score += 2;
        if (ti === 0 || t[ti - 1] === "_" || t[ti - 1] === "-" || t[ti - 1] === " ") {
          score += 3;
        }
        prevMatchIdx = ti;
        qi++;
      }
    }

    // Prefer shorter candidates (more precise matches)
    score -= t.length * 0.1;

    return score;
  }

  // Text autocomplete component for mentions (@) and emojis (:)
  Alpine.data("textAutocomplete", () => ({
    query: "",
    results: [],
    selectedIndex: 0,
    showDropdown: false,
    debounceTimer: null,
    autocompleteType: null, // 'mention' or 'emoji'
    emojiDatabase: null,
    _allEmojis: null,

    init() {
      this._initEmojiDb();
    },

    async _initEmojiDb() {
      if (window.EmojiDatabase) {
        try {
          this.emojiDatabase = new window.EmojiDatabase({
            dataSource: "/assets/libs/emoji-picker-element/data.json",
          });
          await this.emojiDatabase.ready();
          if (this.emojiDatabase._lazyUpdate) {
            this.emojiDatabase._lazyUpdate.catch(() => {});
          }
          // Cache all emojis for fuzzy search
          const allEmojis = [];
          for (let group = 0; group < 10; group++) {
            try {
              const emojis = await this.emojiDatabase.getEmojiByGroup(group);
              allEmojis.push(...emojis);
            } catch (_) {
              // group doesn't exist, stop
            }
          }
          this._allEmojis = allEmojis;
        } catch (e) {
          this.emojiDatabase = null;
          setTimeout(() => this._initEmojiDb(), 1000);
        }
      } else {
        setTimeout(() => this._initEmojiDb(), 100);
      }
    },

    handleInput(event) {
      const textarea = event.target;
      const cursorPos = textarea.selectionStart;
      const textBeforeCursor = textarea.value.substring(0, cursorPos);

      const atMatch = textBeforeCursor.match(/@(\w*)$/);
      if (atMatch) {
        this.autocompleteType = "mention";
        this.query = atMatch[1];
        this.showDropdown = true;
        this.searchProfiles();
        return;
      }

      const emojiMatch = textBeforeCursor.match(/:([a-zA-Z0-9_]{2,})$/);
      if (emojiMatch) {
        this.autocompleteType = "emoji";
        this.query = emojiMatch[1];
        this.showDropdown = true;
        this.searchEmojis();
        return;
      }

      this.showDropdown = false;
      this.autocompleteType = null;
    },

    searchProfiles() {
      clearTimeout(this.debounceTimer);
      this.debounceTimer = setTimeout(async () => {
        try {
          const response = await fetch(
            `/search/profiles?q=${encodeURIComponent(this.query)}`,
          );
          this.results = await response.json();
          this.selectedIndex = 0;
        } catch (error) {
          this.results = [];
        }
      }, 300);
    },

    async searchEmojis() {
      if (!this._allEmojis) {
        this.results = [];
        return;
      }

      clearTimeout(this.debounceTimer);
      this.debounceTimer = setTimeout(() => {
        const q = this.query.toLowerCase();
        const scored = [];

        for (const e of this._allEmojis) {
          const shortcodes = e.shortcodes || [];
          const annotation = (e.annotation || "").toLowerCase();
          let score = 0;

          for (const sc of shortcodes) {
            score = Math.max(score, fuzzyMatch(q, sc));
          }
          score = Math.max(score, fuzzyMatch(q, annotation));

          if (score > 0) {
            scored.push({ emoji: e, score });
          }
        }

        scored.sort((a, b) => b.score - a.score);
        this.results = scored.slice(0, 8).map(({ emoji: e }) => ({
          type: "emoji",
          emoji: e.unicode,
          shortcode: (e.shortcodes && e.shortcodes[0]) || e.annotation,
        }));
        this.selectedIndex = 0;
      }, 50);
    },

    selectResult(result) {
      const textarea = this.$root.querySelector("textarea");
      if (!textarea) return;

      const cursorPos = textarea.selectionStart;
      const textBeforeCursor = textarea.value.substring(0, cursorPos);
      const textAfterCursor = textarea.value.substring(cursorPos);

      let insertText, triggerPos;

      if (this.autocompleteType === "mention") {
        triggerPos = textBeforeCursor.lastIndexOf("@");
        insertText = `<rostra:${result.rostra_id}>`;
      } else if (this.autocompleteType === "emoji") {
        triggerPos = textBeforeCursor.lastIndexOf(":");
        insertText = result.emoji;
      }

      const newText =
        textBeforeCursor.substring(0, triggerPos) +
        insertText +
        textAfterCursor;

      textarea.value = newText;

      const newCursorPos = triggerPos + insertText.length;
      textarea.setSelectionRange(newCursorPos, newCursorPos);

      textarea.dispatchEvent(new Event("input", { bubbles: true }));

      this.showDropdown = false;
      this.autocompleteType = null;
    },

    handleKeydown(event) {
      if (!this.showDropdown) return;

      if (
        event.key === "ArrowDown" ||
        (event.key === "Tab" && !event.shiftKey)
      ) {
        event.preventDefault();
        if (this.results.length > 0) {
          this.selectedIndex = Math.min(
            this.selectedIndex + 1,
            this.results.length - 1,
          );
        }
      } else if (
        event.key === "ArrowUp" ||
        (event.key === "Tab" && event.shiftKey)
      ) {
        event.preventDefault();
        if (this.results.length > 0) {
          this.selectedIndex = Math.max(this.selectedIndex - 1, 0);
        }
      } else if (event.key === "Enter" && this.results.length > 0) {
        event.preventDefault();
        this.selectResult(this.results[this.selectedIndex]);
      } else if (event.key === "Escape") {
        event.preventDefault();
        this.showDropdown = false;
      }
    },
  }));
});

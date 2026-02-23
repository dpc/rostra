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
};

// =============================================================================
// DOMContentLoaded handlers
// =============================================================================

document.addEventListener("DOMContentLoaded", () => {
  // Prevent flickering of images when they are already in the cache
  const images = document.querySelectorAll('img[loading="lazy"]');
  images.forEach((img) => {
    const testImg = new Image();
    testImg.src = img.src;
    if (testImg.complete) {
      img.removeAttribute("loading");
    }
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

  // Text autocomplete component for mentions (@) and emojis (:)
  Alpine.data("textAutocomplete", () => ({
    query: "",
    results: [],
    selectedIndex: 0,
    showDropdown: false,
    debounceTimer: null,
    autocompleteType: null, // 'mention' or 'emoji'
    emojiDatabase: null,

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
      if (!this.emojiDatabase) {
        this.results = [];
        return;
      }

      clearTimeout(this.debounceTimer);
      this.debounceTimer = setTimeout(async () => {
        try {
          await this.emojiDatabase.ready();
          const emojis = await this.emojiDatabase.getEmojiBySearchQuery(
            this.query,
          );
          const q = this.query.toLowerCase();

          const scored = emojis.map((e) => {
            const shortcodes = e.shortcodes || [];
            const annotation = (e.annotation || "").toLowerCase();
            let score = 0;

            for (const sc of shortcodes) {
              const scLower = sc.toLowerCase();
              if (scLower === q) {
                score = Math.max(score, 100);
              } else if (scLower.startsWith(q)) {
                score = Math.max(score, 80);
              } else if (scLower.includes(q)) {
                score = Math.max(score, 40);
              }
            }

            if (annotation.startsWith(q)) {
              score = Math.max(score, 60);
            } else if (annotation.includes(q)) {
              score = Math.max(score, 30);
            }

            return { emoji: e, score };
          });

          scored.sort((a, b) => b.score - a.score);
          this.results = scored.slice(0, 8).map(({ emoji: e }) => ({
            type: "emoji",
            emoji: e.unicode,
            shortcode: (e.shortcodes && e.shortcodes[0]) || e.annotation,
          }));
          this.selectedIndex = 0;
        } catch (error) {
          this.results = [];
        }
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

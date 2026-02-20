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

// Catch unhandled promise rejections (network errors)
window.addEventListener("unhandledrejection", (event) => {
  console.log("Unhandled rejection:", event);
  if (
    event.reason instanceof TypeError &&
    (event.reason.message.includes("NetworkError") ||
      event.reason.message.includes("fetch"))
  ) {
    window.dispatchEvent(
      new CustomEvent("notify", {
        detail: {
          type: "error",
          message: "\u26a0 Network Error - Unable to complete request",
        },
      }),
    );
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
});

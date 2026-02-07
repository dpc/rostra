// Text autocomplete Alpine.js component for mentions (@) and emojis (:)
(function () {
  const componentDef = () => ({
    query: "",
    results: [],
    selectedIndex: 0,
    showDropdown: false,
    debounceTimer: null,
    autocompleteType: null, // 'mention' or 'emoji'
    emojiDatabase: null,

    init() {
      // Initialize emoji database once it's available
      // (loaded by the module script above)
      this._initEmojiDb();
    },

    _initEmojiDb() {
      if (window.EmojiDatabase) {
        this.emojiDatabase = new window.EmojiDatabase({
          dataSource: "/assets/libs/emoji-picker-element/data.json",
        });
      } else {
        // Module hasn't loaded yet, retry shortly
        setTimeout(() => this._initEmojiDb(), 100);
      }
    },

    handleInput(event) {
      const textarea = event.target;
      const cursorPos = textarea.selectionStart;
      const textBeforeCursor = textarea.value.substring(0, cursorPos);

      // Check for @ mention trigger
      const atMatch = textBeforeCursor.match(/@(\w*)$/);
      if (atMatch) {
        this.autocompleteType = "mention";
        this.query = atMatch[1];
        this.showDropdown = true;
        this.searchProfiles();
        return;
      }

      // Check for : emoji trigger (require at least 2 chars after colon)
      const emojiMatch = textBeforeCursor.match(/:([a-zA-Z0-9_]{2,})$/);
      if (emojiMatch) {
        this.autocompleteType = "emoji";
        this.query = emojiMatch[1];
        this.showDropdown = true;
        this.searchEmojis();
        return;
      }

      // No match, hide dropdown
      this.showDropdown = false;
      this.autocompleteType = null;
    },

    searchProfiles() {
      clearTimeout(this.debounceTimer);
      this.debounceTimer = setTimeout(async () => {
        try {
          const response = await fetch(
            `/search/profiles?q=${encodeURIComponent(this.query)}`
          );
          this.results = await response.json();
          this.selectedIndex = 0;
        } catch (error) {
          console.error("Failed to search profiles:", error);
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
      // Small delay to avoid excessive re-renders during fast typing
      this.debounceTimer = setTimeout(async () => {
        try {
          await this.emojiDatabase.ready();
          const emojis = await this.emojiDatabase.getEmojiBySearchQuery(
            this.query
          );
          const q = this.query.toLowerCase();

          // Score and sort results for better relevance
          const scored = emojis.map((e) => {
            const shortcodes = e.shortcodes || [];
            const annotation = (e.annotation || "").toLowerCase();
            let score = 0;

            // Check shortcodes for matches
            for (const sc of shortcodes) {
              const scLower = sc.toLowerCase();
              if (scLower === q) {
                score = Math.max(score, 100); // Exact match
              } else if (scLower.startsWith(q)) {
                score = Math.max(score, 80); // Prefix match
              } else if (scLower.includes(q)) {
                score = Math.max(score, 40); // Substring match
              }
            }

            // Check annotation
            if (annotation.startsWith(q)) {
              score = Math.max(score, 60);
            } else if (annotation.includes(q)) {
              score = Math.max(score, 30);
            }

            return { emoji: e, score };
          });

          // Sort by score descending, take top 8
          scored.sort((a, b) => b.score - a.score);
          this.results = scored.slice(0, 8).map(({ emoji: e }) => ({
            type: "emoji",
            emoji: e.unicode,
            shortcode: (e.shortcodes && e.shortcodes[0]) || e.annotation,
          }));
          this.selectedIndex = 0;
        } catch (error) {
          console.error("Failed to search emojis:", error);
          this.results = [];
        }
      }, 50);
    },

    selectResult(result) {
      const textarea = this.$root.querySelector("textarea");
      if (!textarea) {
        console.error("Textarea not found");
        return;
      }

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
        textBeforeCursor.substring(0, triggerPos) + insertText + textAfterCursor;

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
            this.results.length - 1
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
  });

  // Register component - handle both cases:
  // 1. Alpine already initialized (e.g., on AJAX navigation)
  // 2. Alpine not yet initialized (first page load)
  if (window.Alpine) {
    Alpine.data("textAutocomplete", componentDef);
  } else {
    document.addEventListener("alpine:init", () => {
      Alpine.data("textAutocomplete", componentDef);
    });
  }
})();

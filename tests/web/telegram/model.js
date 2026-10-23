// Shared synthetic data for the Telegram Web A / K fixtures. No network, no account.
// Query flags (both pages), comma separated in ?f=
//   incoming  a new message arrives in the open chat every 3 s (A and K have NO live region for it)
//   flood     30 messages arrive in the open chat within 2 s, once, 5 s after the page loads
//   churn     the chat list re-sorts every 2 s: a preview changes, the unread badge grows, the row
//             moves to the top (DOM reorder, as Teact keyed lists do)
//   typing    the header status flips "typing..." / "online" every 1.5 s
//   noscrollfix  A: do not restore the scroll offset after prepending older messages
// ?chats=N (default 300) and ?msgs=N (default 400) size the data.
(function () {
  const Q = new URLSearchParams(location.search);
  const F = new Set((Q.get("f") || "").split(",").filter(Boolean));
  const NAMES = ["Alice", "Bob", "Carol", "Dave", "Erin", "Frank", "Grace", "Heidi", "Ivan", "Judy", "Mallory", "Niaj",
    "Olivia", "Peggy", "Rupert", "Sybil", "Trent", "Victor", "Walter", "Zoe"];
  const WORDS = ["see you soon", "ok thanks", "lunch at noon?", "look at this", "done", "call me when free", "on my way",
    "sounds good", "did you see the news", "happy birthday", "meeting moved to five", "sending the file now"];
  const NCHATS = +(Q.get("chats") || 300);
  const NMSG = +(Q.get("msgs") || 400);
  function fmt(mins) { return String(Math.floor(mins / 60) % 24).padStart(2, "0") + ":" + String(mins % 60).padStart(2, "0"); }
  const chats = [];
  for (let i = 0; i < NCHATS; i++) {
    chats.push({
      id: "c" + i, name: NAMES[i % NAMES.length] + (i >= NAMES.length ? " " + (Math.floor(i / NAMES.length) + 1) : ""),
      last: WORDS[i % WORDS.length], time: fmt(10 * 60 + ((NCHATS - i) % 600)), unread: i % 7 === 0 ? (i % 5) + 1 : 0,
      pinned: i < 2, muted: i % 11 === 3,
    });
  }
  const store = {};
  let nextId = 1000;
  function history(chat) {
    if (!store[chat.id]) {
      const arr = [];
      for (let i = 1; i <= NMSG; i++) {
        arr.push({
          id: i, out: i % 3 === 0, sender: i % 3 === 0 ? "You" : chat.name,
          text: "Message " + i + " from " + chat.name + ": " + WORDS[(i + chat.id.length) % WORDS.length],
          time: fmt(8 * 60 + i), day: "Day " + (1 + Math.floor((i - 1) / 40)),
        });
      }
      store[chat.id] = arr;
    }
    return store[chat.id];
  }
  function newMessage(chat, out, text) {
    const arr = history(chat);
    const last = arr[arr.length - 1];
    nextId++;
    const m = {
      id: nextId, out, sender: out ? "You" : chat.name, text: text || ("Incoming " + nextId + ": " + WORDS[nextId % WORDS.length]),
      time: fmt(8 * 60 + 500 + (nextId % 300)), day: last ? last.day : "Day 1",
    };
    arr.push(m);
    return m;
  }
  window.TG = { Q, F, chats, history, newMessage, fmt, WORDS };
})();

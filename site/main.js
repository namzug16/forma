for (const button of document.querySelectorAll(".copy-button")) {
  button.addEventListener("click", async () => {
    const text = button.parentElement.querySelector("pre").innerText;
    try {
      await navigator.clipboard.writeText(text);
      button.textContent = "COPIED";
    } catch {
      button.textContent = "COPY FAILED";
    }
    setTimeout(() => (button.textContent = "COPY"), 1500);
  });
}

const revealObserver = new IntersectionObserver((entries) => {
  for (const entry of entries) {
    if (!entry.isIntersecting) continue;
    entry.target.classList.add("is-visible");
    revealObserver.unobserve(entry.target);
  }
}, { threshold: 0.2 });

for (const element of document.querySelectorAll(".reveal")) revealObserver.observe(element);

// The flow field from the artwork example, drawn the same way as flow.lua and animated with anime.js.
function seededRandom(seed) {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let value = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    value ^= value + Math.imul(value ^ (value >>> 7), 61 | value);
    return ((value ^ (value >>> 14)) >>> 0) / 4294967296;
  };
}

function perlinNoise(random) {
  const shuffled = [...Array(256).keys()];
  for (let index = 255; index > 0; index--) {
    const other = Math.floor(random() * (index + 1));
    [shuffled[index], shuffled[other]] = [shuffled[other], shuffled[index]];
  }
  const permutation = [...shuffled, ...shuffled];
  const fade = (t) => t * t * t * (t * (t * 6 - 15) + 10);
  const lerp = (from, to, t) => from + t * (to - from);
  const gradient = (hash, x, y) => (hash & 1 ? -x : x) + (hash & 2 ? -y : y);
  return (x, y) => {
    const cellX = Math.floor(x) & 255, cellY = Math.floor(y) & 255;
    x -= Math.floor(x);
    y -= Math.floor(y);
    const a = permutation[cellX] + cellY, b = permutation[cellX + 1] + cellY;
    return lerp(
      lerp(gradient(permutation[a], x, y), gradient(permutation[b], x - 1, y), fade(x)),
      lerp(gradient(permutation[a + 1], x, y - 1), gradient(permutation[b + 1], x - 1, y - 1), fade(x)),
      fade(y),
    );
  };
}

const artworkFrame = document.querySelector(".artwork-frame");
if (artworkFrame) {
  const random = seededRandom(7);
  const noise = perlinNoise(random);
  for (let line = 0; line < 600; line++) {
    let x = random() * 1080, y = random() * 1080;
    let points = `M${x.toFixed(1)} ${y.toFixed(1)}`;
    for (let step = 0; step < 40; step++) {
      const angle = noise(x / 300, y / 300) * Math.PI * 4;
      x += Math.cos(angle) * 8;
      y += Math.sin(angle) * 8;
      points += `L${x.toFixed(1)} ${y.toFixed(1)}`;
    }
    const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
    path.setAttribute("d", points);
    artworkFrame.append(path);
  }

  if (!matchMedia("(prefers-reduced-motion: reduce)").matches) {
    const { animate, svg, stagger } = anime;
    // Each line grows from its start, then retracts toward its end, so the field looks like it streams.
    animate(svg.createDrawable(artworkFrame.querySelectorAll("path")), {
      draw: ["0 0", "0 1", "1 1"],
      ease: "inOutSine",
      duration: 3000,
      delay: stagger(6),
      loop: true,
    });
  }
}

// The Figma section: render, export as SVG, paste, then a cursor drags the pasted layers around.
const figmaPaste = document.querySelector(".figma-paste");
if (figmaPaste && !matchMedia("(prefers-reduced-motion: reduce)").matches) {
  const { createTimeline } = anime;
  const sun = figmaPaste.querySelector(".figma-sun");
  const headline = figmaPaste.querySelector(".figma-headline");
  const cursor = figmaPaste.querySelector(".figma-cursor");
  const select = (layer) => () => layer.classList.add("is-selected");
  const deselect = (layer) => () => layer.classList.remove("is-selected");

  // Cursor and layers share the paste's percent coordinates, so a drag moves both by the same amount.
  createTimeline({ loop: true, defaults: { ease: "inOutQuad", duration: 400 } })
    .set(".figma-line", { opacity: 0 })
    .set(figmaPaste, { opacity: 0, scale: 0.94 })
    .set(sun, { left: "58%", top: "14%" })
    .set(headline, { left: "14%", top: "60%" })
    .set(cursor, { left: "110%", top: "110%", opacity: 0 })
    .add(".figma-line-1", { opacity: 1 }, 300)
    .add(".figma-line-2", { opacity: 1 }, 1100)
    .add(".figma-line-3", { opacity: 1 }, 2200)
    .add(".figma-line-4", { opacity: 1 }, 3000)
    .add(".figma-line-5", { opacity: 1 }, 3600)
    .add(figmaPaste, { opacity: 1, scale: 1, duration: 500, ease: "outBack" }, 4200)
    .add(cursor, { opacity: 1, duration: 200 }, 4800)
    .add(cursor, { left: "72%", top: "28%", duration: 800 }, 4800)
    .call(select(sun), 5700)
    .add(cursor, { left: "26%", top: "26%", duration: 1000 }, 5900)
    .add(sun, { left: "12%", top: "12%", duration: 1000 }, 5900)
    .call(deselect(sun), 7000)
    .add(cursor, { left: "22%", top: "66%", duration: 800 }, 7200)
    .call(select(headline), 8100)
    .add(cursor, { left: "48%", top: "72%", duration: 1000 }, 8300)
    .add(headline, { left: "40%", top: "66%", duration: 1000 }, 8300)
    .call(deselect(headline), 9400)
    .add(cursor, { left: "110%", top: "110%", opacity: 0, duration: 700 }, 9800)
    .add([figmaPaste, ".figma-line"], { opacity: 0, duration: 500 }, 11500)
    .add({ duration: 400 }, 12000);
}

// enhance.js — progressive enhancement, dimuat di akhir setiap halaman.
// Menandai tautan navigasi yang aktif sesuai URL saat ini.
(function () {
  const here = location.pathname;
  document.querySelectorAll(".nav a").forEach((link) => {
    if (link.getAttribute("href") === here) {
      link.style.color = "var(--text)";
      link.setAttribute("aria-current", "page");
    }
  });
})();

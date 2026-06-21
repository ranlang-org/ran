// Client-side script, served natively by the Ran web server.
const button = document.getElementById("ping");
const status = document.getElementById("status");

button.addEventListener("click", async () => {
  status.textContent = "checking...";
  try {
    const res = await fetch("/api/health");
    const data = await res.json();
    status.textContent = "API status: " + data.status;
  } catch (err) {
    status.textContent = "request failed";
  }
});

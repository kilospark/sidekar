(function () {
  const form = document.getElementById("approve-form");
  const authLoading = document.getElementById("auth-loading");
  const codeInput = document.getElementById("code-input");
  const submitBtn = document.getElementById("submit-btn");
  const message = document.getElementById("message");

  // Check authentication
  fetch("/api/auth/me")
    .then(function (res) {
      if (res.status === 401) {
        // Not logged in — redirect to GitHub OAuth, then back here
        window.location.href = "/api/auth/github?redirect=/approve";
        return;
      }
      if (!res.ok) throw new Error("Auth check failed");
      return res.json();
    })
    .then(function (data) {
      if (!data) return; // redirecting
      authLoading.style.display = "none";
      form.style.display = "block";

      // Pre-fill from URL params (e.g., /approve?code=ABCD-1234)
      var params = new URLSearchParams(window.location.search);
      var prefill = params.get("code");
      if (prefill) {
        codeInput.value = prefill.toUpperCase();
      }
      codeInput.focus();
    })
    .catch(function (err) {
      authLoading.textContent = "Failed to check authentication. Please refresh.";
    });

  // Auto-insert dash after 4 characters
  codeInput.addEventListener("input", function () {
    var val = codeInput.value.replace(/[^A-Za-z0-9]/g, "").toUpperCase();
    if (val.length > 4) {
      val = val.slice(0, 4) + "-" + val.slice(4, 8);
    }
    codeInput.value = val;
  });

  form.addEventListener("submit", function (e) {
    e.preventDefault();

    var code = codeInput.value.trim();
    if (!code || code.length < 9) {
      message.textContent = "Please enter a valid code (e.g., ABCD-1234)";
      message.className = "message error";
      return;
    }

    submitBtn.disabled = true;
    submitBtn.textContent = "Approving...";
    message.textContent = "";
    message.className = "message";

    fetch("/api/auth/device/approve", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ user_code: code }),
    })
      .then(function (res) {
        return res.json().then(function (data) {
          return { ok: res.ok, data: data };
        });
      })
      .then(function (result) {
        if (result.ok) {
          message.textContent = "Device approved! You can close this page.";
          message.className = "message success";
          submitBtn.textContent = "Approved";
          codeInput.disabled = true;
        } else {
          message.textContent = result.data.error || "Approval failed";
          message.className = "message error";
          submitBtn.disabled = false;
          submitBtn.textContent = "Approve Device";
        }
      })
      .catch(function () {
        message.textContent = "Network error. Please try again.";
        message.className = "message error";
        submitBtn.disabled = false;
        submitBtn.textContent = "Approve Device";
      });
  });
})();

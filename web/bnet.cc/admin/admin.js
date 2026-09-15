// Ask before a form marked data-confirm is sent (the admin's Content-Security-Policy forbids
// inline handlers).
document.addEventListener('submit', function (event) {
  var form = event.target;
  var question = form.getAttribute && form.getAttribute('data-confirm');
  if (question && !window.confirm(question)) {
    event.preventDefault();
  }
});

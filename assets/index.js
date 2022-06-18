/**
 * @typedef {object} PathItem
 * @property {"Dir"|"SymlinkDir"|"File"|"SymlinkFile"} path_type
 * @property {string} name
 * @property {number} mtime
 * @property {number} size
 */

// https://stackoverflow.com/a/901144/3642588
const params = new Proxy(new URLSearchParams(window.location.search), {
  get: (searchParams, prop) => searchParams.get(prop),
});

const dirEmptyNote = params.q ? 'No results' : DATA.dir_exists ? 'Empty folder' : 'Folder will be created when a file is uploaded';

/**
 * @type Element
 */
let $pathsTable;
/**
 * @type Element
 */
let $pathsTableBody;
/**
 * @type Element
 */
let $uploadersTable;
/**
 * @type Element
 */
let $emptyFolder;
/**
 * @type string
 */
let baseDir;

class Uploader {
  /**
   * @type number
   */
  idx;
  /**
   * @type File
   */
  file;
  /**
   * @type string
   */
  name;
  /**
   * @type Element
   */
  $uploadStatus;
  /**
   * @type number
   */
  uploaded = 0;
  /**
   * @type number
   */
  lastUptime = 0;
  static globalIdx = 0;
  constructor(file, dirs) {
    this.name = [...dirs, file.name].join("/");
    this.idx = Uploader.globalIdx++;
    this.file = file;
  }

  upload() {
    const { file, idx, name } = this;
    let url = getUrl(name);
    $uploadersTable.insertAdjacentHTML("beforeend", `
  <tr id="upload${idx}" class="uploader">
    <td class="path cell-name">
      ${getSvg("File")}
      <a href="${url}">${name}</a>
    </td>
    <td class="cell-status upload-status" id="uploadStatus${idx}"></td>
  </tr>`);
    $uploadersTable.classList.remove("hidden");
    $emptyFolder.classList.add("hidden");
    this.$uploadStatus = document.getElementById(`uploadStatus${idx}`);
    this.lastUptime = Date.now();

    const ajax = new XMLHttpRequest();
    ajax.upload.addEventListener("progress", e => this.progress(e), false);
    ajax.addEventListener("readystatechange", () => {
      if(ajax.readyState === 4) {
        if (ajax.status >= 200 && ajax.status < 300) {
          this.complete();
        } else {
          this.fail();
        }
      }
    })
    ajax.addEventListener("error", () => this.fail(), false);
    ajax.addEventListener("abort", () => this.fail(), false);
    ajax.open("PUT", url);
    ajax.send(file);
  }

  progress(event) {
    let now = Date.now();
    let speed = (event.loaded - this.uploaded) / (now - this.lastUptime) * 1000;
    let [speedValue, speedUnit] = formatSize(speed);
    const speedText = `${speedValue}${speedUnit.toLowerCase()}/s`;
    const progress = formatPercent((event.loaded / event.total) * 100);
    const duration = formatDuration((event.total - event.loaded) / speed)
    this.$uploadStatus.innerHTML = `<span>${speedText}</span><span>${progress}</span><span>${duration}</span>`;
    this.uploaded = event.loaded;
    this.lastUptime = now;
  }

  complete() {
    this.$uploadStatus.innerHTML = `✓`;
  }

  fail() {
    this.$uploadStatus.innerHTML = `✗`;
  }
}

/**
 * Add breadcrumb
 * @param {string} value 
 */
function addBreadcrumb(value) {
  const $breadcrumb = document.querySelector(".breadcrumb");
  const parts = value.split("/").filter(v => !!v);
  const len = parts.length;
  let path = "";
  for (let i = 0; i < len; i++) {
    const name = parts[i];
    if (i > 0) {
      path  += "/" + name;
    }
    if (i === len - 1) {
      $breadcrumb.insertAdjacentHTML("beforeend", `<b>${name}</b>`);
      baseDir = name;
    } else if (i === 0) {
      $breadcrumb.insertAdjacentHTML("beforeend", `<a href="/"><b>${name}</b></a>`);
    } else {
      $breadcrumb.insertAdjacentHTML("beforeend", `<a href="${encodeURI(path)}">${name}</a>`);
    }
    $breadcrumb.insertAdjacentHTML("beforeend", `<span class="separator">/</span>`);
  }
}

/**
 * Add pathitem
 * @param {PathItem} file 
 * @param {number} index 
 */
function addPath(file, index) {
  let url = getUrl(file.name)
  let actionDelete = "";
  let actionDownload = "";
  if (file.path_type.endsWith("Dir")) {
    url += "/";
    actionDownload = `
    <div class="action-btn">
      <a href="${url}?zip" title="Download folder as a .zip file">
        <svg width="16" height="16" viewBox="0 0 16 16"><path d="M.5 9.9a.5.5 0 0 1 .5.5v2.5a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-2.5a.5.5 0 0 1 1 0v2.5a2 2 0 0 1-2 2H2a2 2 0 0 1-2-2v-2.5a.5.5 0 0 1 .5-.5z"/><path d="M7.646 11.854a.5.5 0 0 0 .708 0l3-3a.5.5 0 0 0-.708-.708L8.5 10.293V1.5a.5.5 0 0 0-1 0v8.793L5.354 8.146a.5.5 0 1 0-.708.708l3 3z"/></svg>
      </a>
    </div>`;
  } else {
    actionDownload = `
    <div class="action-btn" >
      <a href="${url}" title="Download file" download>
        <svg width="16" height="16" viewBox="0 0 16 16"><path d="M.5 9.9a.5.5 0 0 1 .5.5v2.5a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-2.5a.5.5 0 0 1 1 0v2.5a2 2 0 0 1-2 2H2a2 2 0 0 1-2-2v-2.5a.5.5 0 0 1 .5-.5z"/><path d="M7.646 11.854a.5.5 0 0 0 .708 0l3-3a.5.5 0 0 0-.708-.708L8.5 10.293V1.5a.5.5 0 0 0-1 0v8.793L5.354 8.146a.5.5 0 1 0-.708.708l3 3z"/></svg>
      </a>
    </div>`;
  }
  if (DATA.allow_delete) {
    actionDelete = `
    <div onclick="deletePath(${index})" class="action-btn" id="deleteBtn${index}" title="Delete ${file.name}">
      <svg width="16" height="16" fill="currentColor"viewBox="0 0 16 16"><path d="M6.854 7.146a.5.5 0 1 0-.708.708L7.293 9l-1.147 1.146a.5.5 0 0 0 .708.708L8 9.707l1.146 1.147a.5.5 0 0 0 .708-.708L8.707 9l1.147-1.146a.5.5 0 0 0-.708-.708L8 8.293 6.854 7.146z"/><path d="M14 14V4.5L9.5 0H4a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2zM9.5 3A1.5 1.5 0 0 0 11 4.5h2V14a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V2a1 1 0 0 1 1-1h5.5v2z"/></svg>
    </div>`;
  }
  let actionCell = `
  <td class="cell-actions">
    ${actionDownload}
    ${actionDelete}
  </td>`

  $pathsTableBody.insertAdjacentHTML("beforeend", `
<tr id="addPath${index}">
  <td class="path cell-icon">
    ${getSvg(file.path_type)}
  </td>
  <td class="path cell-name">
    <a href="${url}" title="${file.name}">${file.name}</a>
  </td>
  <td class="cell-mtime">${formatMtime(file.mtime)}</td>
  <td class="cell-size">${formatSize(file.size).join(" ")}</td>
  ${actionCell}
</tr>`)
}

/**
 * Delete pathitem
 * @param {number} index 
 * @returns 
 */
async function deletePath(index) {
  const file = DATA.paths[index];
  if (!file) return;

  if (!confirm(`Delete \`${file.name}\`?`)) return;

  try {
    const res = await fetch(getUrl(file.name), {
      method: "DELETE",
    });
    if (res.status >= 200 && res.status < 300) {
        document.getElementById(`addPath${index}`).remove();
        DATA.paths[index] = null;
        if (!DATA.paths.find(v => !!v)) {
          $pathsTable.classList.add("hidden");
          $emptyFolder.textContent = dirEmptyNote;
          $emptyFolder.classList.remove("hidden");
        }
    } else {
      throw new Error(await res.text())
    }
  } catch (err) {
    alert(`Cannot delete \`${file.name}\`, ${err.message}`);
  }
}

function dropzone() {
    ["drag", "dragstart", "dragend", "dragover", "dragenter", "dragleave", "drop"].forEach(name => {
      document.addEventListener(name, e => {
          e.preventDefault();
          e.stopPropagation();
      });
    });
    document.addEventListener("drop", e => {
      if (!e.dataTransfer.items[0].webkitGetAsEntry) {
        const files = e.dataTransfer.files.filter(v => v.size > 0);
        for (const file of files) {
          new Uploader(file, []).upload();
        }
      } else {
        const entries = [];
        const len = e.dataTransfer.items.length;
        for (let i = 0; i < len; i++) {
          entries.push(e.dataTransfer.items[i].webkitGetAsEntry());
        }
        addFileEntries(entries, [])
      }
    });
}

async function addFileEntries(entries, dirs) {
  for (const entry of entries) {
    if (entry.isFile) {
      entry.file(file => {
        new Uploader(file, dirs).upload();
      });
    } else if (entry.isDirectory) {
      const dirReader = entry.createReader()
      dirReader.readEntries(entries => addFileEntries(entries, [...dirs, entry.name]));
    }
  }
}


function getUrl(name) {
    let url = location.href.split('?')[0];
    if (!url.endsWith("/")) url += "/";
    url += encodeURI(name);
    return url;
}

function getSvg(path_type) {
  switch (path_type) {
    case "Dir":
      return `<svg height="16" viewBox="0 0 14 16" width="14"><path fill-rule="evenodd" d="M13 4H7V3c0-.66-.31-1-1-1H1c-.55 0-1 .45-1 1v10c0 .55.45 1 1 1h12c.55 0 1-.45 1-1V5c0-.55-.45-1-1-1zM6 4H1V3h5v1z"></path></svg>`;
    case "File":
      return `<svg height="16" viewBox="0 0 12 16" width="12"><path fill-rule="evenodd" d="M6 5H2V4h4v1zM2 8h7V7H2v1zm0 2h7V9H2v1zm0 2h7v-1H2v1zm10-7.5V14c0 .55-.45 1-1 1H1c-.55 0-1-.45-1-1V2c0-.55.45-1 1-1h7.5L12 4.5zM11 5L8 2H1v12h10V5z"></path></svg>`;
    case "SymlinkDir":
      return `<svg height="16" viewBox="0 0 14 16" width="14"><path fill-rule="evenodd" d="M13 4H7V3c0-.66-.31-1-1-1H1c-.55 0-1 .45-1 1v10c0 .55.45 1 1 1h12c.55 0 1-.45 1-1V5c0-.55-.45-1-1-1zM1 3h5v1H1V3zm6 9v-2c-.98-.02-1.84.22-2.55.7-.71.48-1.19 1.25-1.45 2.3.02-1.64.39-2.88 1.13-3.73C4.86 8.43 5.82 8 7.01 8V6l4 3-4 3H7z"></path></svg>`;
    default:
      return `<svg height="16" viewBox="0 0 12 16" width="12"><path fill-rule="evenodd" d="M8.5 1H1c-.55 0-1 .45-1 1v12c0 .55.45 1 1 1h10c.55 0 1-.45 1-1V4.5L8.5 1zM11 14H1V2h7l3 3v9zM6 4.5l4 3-4 3v-2c-.98-.02-1.84.22-2.55.7-.71.48-1.19 1.25-1.45 2.3.02-1.64.39-2.88 1.13-3.73.73-.84 1.69-1.27 2.88-1.27v-2H6z"></path></svg>`;
  }
}

function formatMtime(mtime) {
  if (!mtime) return ""
  const date = new Date(mtime);
  const year = date.getFullYear();
  const month = padZero(date.getMonth() + 1, 2);
  const day = padZero(date.getDate(), 2);
  const hours = padZero(date.getHours(), 2);
  const minutes = padZero(date.getMinutes(), 2);
  return `${year}-${month}-${day} ${hours}:${minutes}`;
}

function padZero(value, size) {
  return ("0".repeat(size) + value).slice(-1 * size)
}

function formatSize(size) {
  if (!size) return []
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
  if (size == 0) return [0, "Byte"];
  const i = parseInt(Math.floor(Math.log(size) / Math.log(1024)));
  return [Math.round(size / Math.pow(1024, i), 2), sizes[i]];
}

function formatDuration(seconds) {
  seconds = Math.ceil(seconds);
  let h = Math.floor(seconds / 3600);
  let m = Math.floor((seconds - h * 3600) / 60);
  let s = seconds - h * 3600 - m * 60
  return `${padZero(h, 2)}:${padZero(m, 2)}:${padZero(s, 2)}`;
}

function formatPercent(precent) {
  if (precent > 10) {
    return precent.toFixed(1) + "%";
  } else {
    return precent.toFixed(2) + "%";
  }
}

function ready() {
  $pathsTable = document.querySelector(".paths-table")
  $pathsTableBody = document.querySelector(".paths-table tbody");
  $uploadersTable = document.querySelector(".uploaders-table");
  $emptyFolder = document.querySelector(".empty-folder");

  if (params.q) {
    document.getElementById('search').value = params.q;
  }

  addBreadcrumb(DATA.breadcrumb);
  if (Array.isArray(DATA.paths)) {
    const len = DATA.paths.length;
    if (len > 0) {
      $pathsTable.classList.remove("hidden");
    }
    for (let i = 0; i < len; i++) {
      addPath(DATA.paths[i], i);
    }
    if (len == 0) {
      $emptyFolder.textContent = dirEmptyNote;
      $emptyFolder.classList.remove("hidden");
    }
  }
  if (DATA.allow_upload) {
    dropzone();
    document.querySelector(".upload-control").classList.remove("hidden");
    document.getElementById("file").addEventListener("change", e => {
      const files = e.target.files;
      for (let file of files) {
        new Uploader(file, []).upload();
      }
    });
  }
}

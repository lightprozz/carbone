#!/usr/bin/env node
'use strict';

/**
 * HTTP service around the Carbone Community Edition renderer, so the library can
 * run as a long-lived process on Railway (or any container host).
 *
 *   GET  /health   liveness probe, never requires the API token
 *   GET  /         service description
 *   POST /render   render a template
 *
 * POST /render accepts two content types:
 *
 *   application/json
 *     { "template": <base64 file>, "templateName": "report.docx",
 *       "data": {}, "options": { "convertTo": "pdf" } }
 *
 *   multipart/form-data
 *     template       file    the template itself
 *     data           text    the JSON data
 *     options        text    carbone.render options
 *     templateName   text    optional, defaults to the uploaded file name
 *
 * When CARBONE_API_TOKEN is set, /render requires it as either
 * `Authorization: Bearer <token>` or `x-api-token: <token>`.
 *
 * Environment: PORT (default 3000), MAX_BODY_MB (default 20),
 *              CARBONE_WORK_DIR (default <tmp>/carbone),
 *              CARBONE_API_TOKEN (optional: empty, no authentication).
 */

var crypto = require('crypto');
var fs = require('fs');
var http = require('http');
var os = require('os');
var path = require('path');
var which = require('which');
var carbone = require('./lib');

var PORT = parseInt(process.env.PORT, 10) || 3000;
var MAX_BODY_BYTES = (parseInt(process.env.MAX_BODY_MB, 10) || 20) * 1024 * 1024;
var API_TOKEN = process.env.CARBONE_API_TOKEN || '';

var MIME_TYPES = {
  docx  : 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
  xlsx  : 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
  pptx  : 'application/vnd.openxmlformats-officedocument.presentationml.presentation',
  odt   : 'application/vnd.oasis.opendocument.text',
  ods   : 'application/vnd.oasis.opendocument.spreadsheet',
  odp   : 'application/vnd.oasis.opendocument.presentation',
  pdf   : 'application/pdf',
  html  : 'text/html',
  xhtml : 'application/xhtml+xml',
  xml   : 'application/xml',
  csv   : 'text/csv',
  txt   : 'text/plain'
};

// Templates and rendered files are written to a writable directory: the container
// runs as a non-root user, and the library writes templates relative to templatePath.
var workDir = process.env.CARBONE_WORK_DIR || path.join(os.tmpdir(), 'carbone');
// carbone.set only creates the leaf directories, the parent must exist already.
fs.mkdirSync(workDir, { recursive : true, mode : 0o755 });
carbone.set({
  tempPath     : os.tmpdir(),
  templatePath : path.join(workDir, 'templates'),
  renderPath   : path.join(workDir, 'render')
});

function sendJson (res, status, payload) {
  var body = Buffer.from(JSON.stringify(payload));
  res.writeHead(status, { 'Content-Type': 'application/json', 'Content-Length': body.length });
  res.end(body);
}

function sendError (res, status, message) {
  sendJson(res, status, { error : message });
}

/**
 * Check the API token. The comparison is constant-time so that a wrong guess
 * cannot be refined by measuring the response time. Without a configured token
 * every request is allowed, which keeps local development simple.
 */
function isAuthorized (req) {
  if (API_TOKEN === '') {
    return true;
  }
  var _header = String(req.headers.authorization || req.headers['x-api-token'] || '');
  var _token = _header.indexOf('Bearer ') === 0 ? _header.slice(7) : _header;
  var _given = Buffer.from(_token);
  var _expected = Buffer.from(API_TOKEN);
  return _given.length === _expected.length && crypto.timingSafeEqual(_given, _expected);
}

function readBody (req, callback) {
  var chunks = [];
  var size = 0;
  var done = false;

  function finish (err, buffer) {
    if (done === true) {
      return;
    }
    done = true;
    callback(err, buffer);
  }

  req.on('data', function (chunk) {
    size += chunk.length;
    if (size > MAX_BODY_BYTES) {
      req.destroy();
      return finish(Object.assign(new Error('Payload too large'), { status : 413 }));
    }
    chunks.push(chunk);
  });
  req.on('end', function () {
    finish(null, Buffer.concat(chunks));
  });
  req.on('error', function (err) {
    finish(err);
  });
}

/**
 * Parse a multipart/form-data body. Text fields are decoded as utf8; the
 * uploaded file is kept as a Buffer because a document is binary.
 *
 * @param  {Buffer} body
 * @param  {String} boundary
 * @return {Object} { fields : {name : String}, files : {name : {filename, data}} }
 */
function parseMultipart (body, boundary) {
  var _delimiter = Buffer.from('--' + boundary);
  var _crlf = Buffer.from('\r\n');
  var _separator = Buffer.from('\r\n\r\n');
  var _fields = {};
  var _files = {};
  var _cursor = body.indexOf(_delimiter);

  while (_cursor !== -1) {
    var _partStart = _cursor + _delimiter.length;
    // the closing delimiter is followed by '--'
    if (body.slice(_partStart, _partStart + 2).toString() === '--') {
      break;
    }
    if (body.slice(_partStart, _partStart + 2).equals(_crlf) === true) {
      _partStart += 2;
    }
    var _partEnd = body.indexOf(_delimiter, _partStart);
    if (_partEnd === -1) {
      break;
    }
    var _nextCursor = _partEnd;
    // the CRLF before the delimiter belongs to the delimiter, not to the content
    if (body.slice(_partEnd - 2, _partEnd).equals(_crlf) === true) {
      _partEnd -= 2;
    }
    var _part = body.slice(_partStart, _partEnd);
    var _headerEnd = _part.indexOf(_separator);
    if (_headerEnd !== -1) {
      var _headers = _part.slice(0, _headerEnd).toString('utf8');
      var _content = _part.slice(_headerEnd + _separator.length);
      var _name = /name="([^"]*)"/i.exec(_headers);
      var _filename = /filename="([^"]*)"/i.exec(_headers);
      if (_name !== null && _name[1] !== '') {
        if (_filename !== null && _filename[1] !== '') {
          _files[_name[1]] = { filename : _filename[1], data : _content };
        }
        else {
          _fields[_name[1]] = _content.toString('utf8');
        }
      }
    }
    _cursor = _nextCursor;
  }
  return { fields : _fields, files : _files };
}

/**
 * Parse an optional JSON field: an empty or missing field is an empty object.
 * @return {Object|String} the parsed value, or an error message
 */
function parseJsonObject (value, fieldName) {
  if (value === undefined || String(value).trim() === '') {
    return {};
  }
  var _parsed;
  try {
    _parsed = JSON.parse(value);
  }
  catch (parseError) {
    return '"' + fieldName + '" must be valid JSON';
  }
  if (_parsed === null || typeof _parsed !== 'object') {
    return '"' + fieldName + '" must be a JSON object or array';
  }
  return _parsed;
}

function outputExtension (options, fallback) {
  var convertTo = options.convertTo;
  if (typeof convertTo === 'string') {
    return convertTo.toLowerCase();
  }
  if (convertTo && typeof convertTo === 'object' && typeof convertTo.formatName === 'string') {
    return convertTo.formatName.toLowerCase();
  }
  return fallback;
}

function safeFilename (name) {
  var sanitized = String(name).replace(/[^A-Za-z0-9._-]/g, '_').replace(/^\.+/, '').slice(0, 120);
  return sanitized.length > 0 ? sanitized : 'report';
}

/**
 * Render a template and send the document back.
 *
 * @param {Buffer} template
 * @param {String} templateName  used to detect the format of the template
 * @param {Object|Array} data
 * @param {Object} options       carbone.render options
 * @param {Object} res
 */
function renderDocument (template, templateName, data, options, res) {
  if (options.renderPrefix !== undefined && options.renderPrefix !== null) {
    return sendError(res, 400, 'the "renderPrefix" option is not supported by this endpoint');
  }

  var _templateExtension = path.extname(String(templateName || '')).slice(1).toLowerCase();
  var _fileId = 'tpl-' + crypto.randomBytes(8).toString('hex')
    + (_templateExtension.length > 0 ? '.' + _templateExtension : '');

  carbone.addTemplate(_fileId, template, function (addError) {
    if (addError) {
      return sendError(res, 500, 'cannot store the template: ' + addError);
    }

    var removeTemplate = function () {
      carbone.removeTemplate(_fileId, function () { /* the result is already in memory */ });
    };

    try {
      carbone.render(_fileId, data, options, function (renderError, result, reportName) {
        removeTemplate();
        if (renderError) {
          return sendError(res, 422, String(renderError));
        }
        // text templates (html, xhtml, xml) are returned as a string, not a buffer
        if (typeof result === 'string') {
          result = Buffer.from(result, 'utf8');
        }
        if (Buffer.isBuffer(result) === false) {
          return sendError(res, 500, 'unexpected render output');
        }

        var _extension = outputExtension(options, _templateExtension);
        var _filename = safeFilename(reportName || options.reportName || 'report')
          + (_extension.length > 0 ? '.' + _extension : '');
        res.writeHead(200, {
          'Content-Type'        : MIME_TYPES[_extension] || 'application/octet-stream',
          'Content-Length'      : result.length,
          'Content-Disposition' : 'attachment; filename="' + _filename + '"'
        });
        res.end(result);
      });
    }
    catch (renderThrow) {
      removeTemplate();
      sendError(res, 400, String(renderThrow.message || renderThrow));
    }
  });
}

function handleJsonRequest (body, res) {
  var _payload;
  try {
    _payload = JSON.parse(body.toString('utf8'));
  }
  catch (parseError) {
    return sendError(res, 400, 'body must be valid JSON');
  }

  if (_payload === null || typeof _payload !== 'object' || typeof _payload.template !== 'string') {
    return sendError(res, 400, '"template" is required and must be the base64 content of the template file');
  }

  var _template = Buffer.from(_payload.template, 'base64');
  if (_template.length === 0) {
    return sendError(res, 400, '"template" is not valid base64 content');
  }

  var _data = _payload.data === undefined ? {} : _payload.data;
  if (_data === null || typeof _data !== 'object') {
    return sendError(res, 400, '"data" must be a JSON object or array');
  }

  var _options = _payload.options === undefined || _payload.options === null ? {} : _payload.options;
  if (typeof _options !== 'object' || Array.isArray(_options) === true) {
    return sendError(res, 400, '"options" must be a JSON object');
  }

  renderDocument(_template, _payload.templateName, _data, _options, res);
}

function handleMultipartRequest (body, contentType, res) {
  var _boundaryMatch = /boundary=(?:"([^"]+)"|([^;]+))/i.exec(contentType);
  var _boundary = _boundaryMatch === null ? '' : (_boundaryMatch[1] || _boundaryMatch[2] || '').trim();
  if (_boundary === '') {
    return sendError(res, 400, 'multipart/form-data requires a boundary');
  }

  var _form = parseMultipart(body, _boundary);
  if (_form.files.template === undefined) {
    return sendError(res, 400, '"template" is required and must be the uploaded template file');
  }

  var _data = parseJsonObject(_form.fields.data, 'data');
  if (typeof _data === 'string') {
    return sendError(res, 400, _data);
  }
  var _options = parseJsonObject(_form.fields.options, 'options');
  if (typeof _options === 'string') {
    return sendError(res, 400, _options);
  }

  renderDocument(_form.files.template.data,
    _form.fields.templateName || _form.files.template.filename,
    _data, _options, res);
}

function handleRender (req, res) {
  if (isAuthorized(req) === false) {
    res.setHeader('WWW-Authenticate', 'Bearer');
    return sendError(res, 401, 'missing or invalid API token');
  }

  var _contentType = String(req.headers['content-type'] || '');
  readBody(req, function (err, body) {
    if (err) {
      return sendError(res, err.status || 400, err.message);
    }
    if (_contentType.indexOf('multipart/form-data') === 0) {
      return handleMultipartRequest(body, _contentType, res);
    }
    if (_contentType.indexOf('application/json') === 0) {
      return handleJsonRequest(body, res);
    }
    sendError(res, 415, 'content-type must be application/json or multipart/form-data');
  });
}

var server = http.createServer(function (req, res) {
  if (req.method === 'GET' && req.url === '/health') {
    return sendJson(res, 200, { status : 'ok' });
  }
  if (req.method === 'POST' && req.url === '/render') {
    return handleRender(req, res);
  }
  if (req.method === 'GET' && req.url === '/') {
    return sendJson(res, 200, {
      service   : 'carbone',
      version   : require('./package.json').version,
      auth      : API_TOKEN === '' ? 'disabled' : 'bearer token required',
      endpoints : {
        'GET /health' : 'liveness probe',
        'POST /render': 'application/json {template, templateName, data, options} '
          + 'or multipart/form-data {template file, data, options}'
      }
    });
  }
  sendError(res, 404, 'not found');
});

server.listen(PORT, function () {
  var _converter = which.sync('soffice.bin', { nothrow : true }) || which.sync('soffice', { nothrow : true });
  console.log('carbone service listening on port ' + PORT);
  console.log('API token: ' + (API_TOKEN === '' ? 'disabled' : 'required on /render'));
  console.log(_converter
    ? 'LibreOffice found: ' + _converter + ' (format conversion available)'
    : 'LibreOffice not found: only same-format rendering is available');
});

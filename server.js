#!/usr/bin/env node
'use strict';

/**
 * HTTP service around the Carbone Community Edition renderer, so the library can
 * run as a long-lived process on Railway (or any container host).
 *
 *   GET  /health   liveness probe
 *   POST /render   { "template": <base64 file>, "data": {}, "options": {} }
 *                  -> the rendered document
 *
 * Environment: PORT (default 3000), MAX_BODY_MB (default 20),
 *              CARBONE_WORK_DIR (default <tmp>/carbone).
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
  sendJson(res, status, { error: message });
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
 * Extension of the produced document: the requested conversion format wins,
 * otherwise the template format is returned unchanged.
 */
function outputExtension (options, fallback) {
  var convertTo = options && options.convertTo;
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

function handleRender (req, res) {
  readBody(req, function (err, body) {
    if (err) {
      return sendError(res, err.status || 400, err.message);
    }

    var payload;
    try {
      payload = JSON.parse(body.toString('utf8'));
    }
    catch (parseError) {
      return sendError(res, 400, 'body must be valid JSON');
    }

    if (payload === null || typeof payload !== 'object' || typeof payload.template !== 'string') {
      return sendError(res, 400, '"template" is required and must be the base64 content of the template file');
    }

    var template = Buffer.from(payload.template, 'base64');
    if (template.length === 0) {
      return sendError(res, 400, '"template" is not valid base64 content');
    }

    var options = payload.options && typeof payload.options === 'object' ? payload.options : {};
    if (options.renderPrefix !== undefined && options.renderPrefix !== null) {
      return sendError(res, 400, 'the "renderPrefix" option is not supported by this endpoint');
    }

    var templateExtension = path.extname(String(payload.templateName || '')).slice(1).toLowerCase();
    var fileId = 'tpl-' + crypto.randomBytes(8).toString('hex') + (templateExtension.length > 0 ? '.' + templateExtension : '');

    carbone.addTemplate(fileId, template, function (addError) {
      if (addError) {
        return sendError(res, 500, 'cannot store the template: ' + addError);
      }

      var removeTemplate = function () {
        carbone.removeTemplate(fileId, function () { /* the render result is already in memory */ });
      };

      try {
        carbone.render(fileId, payload.data === undefined ? {} : payload.data, options, function (renderError, result, reportName) {
          removeTemplate();
          if (renderError) {
            return sendError(res, 422, String(renderError));
          }
          // Text templates (html, xhtml, xml) are returned as a string, not a buffer.
          if (typeof result === 'string') {
            result = Buffer.from(result, 'utf8');
          }
          if (Buffer.isBuffer(result) === false) {
            return sendError(res, 500, 'unexpected render output');
          }

          var extension = outputExtension(options, templateExtension);
          var filename = safeFilename(reportName || options.reportName || 'report') + (extension.length > 0 ? '.' + extension : '');
          res.writeHead(200, {
            'Content-Type'        : MIME_TYPES[extension] || 'application/octet-stream',
            'Content-Length'      : result.length,
            'Content-Disposition' : 'attachment; filename="' + filename + '"'
          });
          res.end(result);
        });
      }
      catch (renderThrow) {
        removeTemplate();
        sendError(res, 400, String(renderThrow.message || renderThrow));
      }
    });
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
      service  : 'carbone',
      version  : require('./package.json').version,
      endpoints: {
        'GET /health' : 'liveness probe',
        'POST /render': 'body { template: <base64>, data: <object|array>, options: <carbone.render options> }'
      }
    });
  }
  sendError(res, 404, 'not found');
});

server.listen(PORT, function () {
  var converter = which.sync('soffice.bin', { nothrow : true }) || which.sync('soffice', { nothrow : true });
  console.log('carbone service listening on port ' + PORT);
  console.log(converter
    ? 'LibreOffice found: ' + converter + ' (format conversion available)'
    : 'LibreOffice not found: only same-format rendering is available');
});

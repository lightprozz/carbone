'use strict';

/**
 * Render HTML produced by a WYSIWYG editor (TinyMCE, CKEditor, ...) as native
 * document markup: `{d.comment:html}`.
 *
 * The `:html` formatter of Carbone v5 is an Enterprise feature. This formatter
 * fills the gap for the Community Edition by converting a subset of HTML into
 * WordprocessingML (docx) or ODF (odt) markup.
 *
 * The builder XML-escapes a value, then calls the formatters flagged
 * `canInjectXML` and splices their result into the document as raw markup (the
 * mechanism `convCRLF` uses to insert line breaks). The input therefore arrives
 * escaped, and every piece of text emitted here is escaped again by this
 * formatter: no part of the input is passed through as markup.
 *
 * Handled: paragraphs, headings, line breaks, bold, italic, underline,
 * strikethrough, inline code, links (styled as text), bulleted and numbered
 * lists, and HTML entities. Other tags are dropped, their text content is kept.
 */

// Above this size the input is truncated: a document is not a place for a huge payload.
const MAX_INPUT_LENGTH = 200000;

// inline tags and the text style they turn on
const INLINE_TAGS = {
  b      : 'bold',
  strong : 'bold',
  i      : 'italic',
  em     : 'italic',
  u      : 'underline',
  ins    : 'underline',
  s      : 'strike',
  strike : 'strike',
  del    : 'strike',
  code   : 'code',
  a      : 'link'
};

const BLOCK_TAGS = [
  'p', 'div', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'ul', 'ol', 'li', 'blockquote', 'pre',
  'table', 'thead', 'tbody', 'tfoot', 'tr', 'td', 'th', 'section', 'article', 'header',
  'footer', 'main', 'aside', 'figure', 'figcaption', 'address', 'dl', 'dt', 'dd'
];

const VOID_TAGS = ['br', 'hr', 'img', 'input', 'meta', 'link', 'col', 'area', 'base', 'wbr'];

// font size (half-points) of the heading levels, matching Word's defaults
const HEADING_SIZES = { h1 : 32, h2 : 28, h3 : 26, h4 : 24, h5 : 22, h6 : 20 };

const NAMED_ENTITIES = {
  nbsp   : ' ', amp : '&', lt : '<', gt : '>', quot : '"', apos : '\'', hellip : '...',
  mdash  : '\u2014', ndash : '\u2013', lsquo : '\u2018', rsquo : '\u2019', ldquo : '\u201c',
  rdquo  : '\u201d', bull : '\u2022', middot : '\u00b7', deg : '\u00b0', euro : '\u20ac',
  pound  : '\u00a3', yen : '\u00a5', copy : '\u00a9', reg : '\u00ae', trade : '\u2122',
  times  : '\u00d7', divide : '\u00f7', plusmn : '\u00b1', frac12 : '\u00bd',
  agrave : '\u00e0', aacute : '\u00e1', acirc : '\u00e2', atilde : '\u00e3', auml : '\u00e4',
  aring  : '\u00e5', ccedil : '\u00e7', egrave : '\u00e8', eacute : '\u00e9', ecirc : '\u00ea',
  euml   : '\u00eb', igrave : '\u00ec', iacute : '\u00ed', icirc : '\u00ee', iuml : '\u00ef',
  ntilde : '\u00f1', ograve : '\u00f2', oacute : '\u00f3', ocirc : '\u00f4', otilde : '\u00f5',
  ouml   : '\u00f6', oslash : '\u00f8', ugrave : '\u00f9', uacute : '\u00fa', ucirc : '\u00fb',
  uuml   : '\u00fc', yacute : '\u00fd', yuml : '\u00ff'
};

function escapeXml (str) {
  return str
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/[\u0000-\u0008]|[\u000B-\u000C]|[\u000E-\u001F]/g, '');
}

/**
 * Decode the entities of the incoming HTML. Carbone escapes `&`, `<` and `>`
 * before calling this formatter, so `&amp;lt;` has to become `<` and not `&lt;`.
 */
function decodeEntities (str) {
  return str.replace(/&(#x?[0-9a-fA-F]+|[a-zA-Z][a-zA-Z0-9]*);/g, function (match, entity) {
    if (entity.charAt(0) === '#') {
      var _isHex = entity.charAt(1) === 'x' || entity.charAt(1) === 'X';
      var _code = parseInt(_isHex ? entity.slice(2) : entity.slice(1), _isHex ? 16 : 10);
      // ignore out of range and surrogate code points
      if (isNaN(_code) === true || _code < 0 || _code > 0x10FFFF || (_code >= 0xD800 && _code <= 0xDFFF)) {
        return match;
      }
      return String.fromCodePoint(_code);
    }
    var _named = NAMED_ENTITIES[entity];
    return _named === undefined ? match : _named;
  });
}

/**
 * Split HTML into tags and text, dropping the elements whose content is not
 * document text.
 * @return {Array} items: {tag : String|null, closing : Boolean, selfClosing : Boolean, text : String}
 */
function tokenize (html) {
  var _html = html.replace(/<!--[\s\S]*?-->/g, '').replace(/<(script|style|head)[\s\S]*?<\/\1\s*>/gi, '');
  var _tokens = [];
  var _tagRegex = /<(\/?)([a-zA-Z][a-zA-Z0-9]*)((?:"[^"]*"|'[^']*'|[^>])*)>/g;
  var _lastIndex = 0;
  var _match;

  while ((_match = _tagRegex.exec(_html)) !== null) {
    if (_match.index > _lastIndex) {
      _tokens.push({ text : _html.slice(_lastIndex, _match.index) });
    }
    var _name = _match[2].toLowerCase();
    var _selfClosing = _match[3].trim().slice(-1) === '/' || VOID_TAGS.indexOf(_name) !== -1;
    _tokens.push({
      tag         : _name,
      closing     : _match[1] === '/',
      selfClosing : _selfClosing
    });
    _lastIndex = _tagRegex.lastIndex;
  }
  if (_lastIndex < _html.length) {
    _tokens.push({ text : _html.slice(_lastIndex) });
  }
  return _tokens;
}

/**
 * Walk the tokens and call the writer. Inline styles are tracked as counters so
 * that nested tags (a `<b>` inside an `<i>`) combine.
 */
function walk (tokens, writer) {
  var _style = {};
  var _listStack = [];
  var _itemIndex = 0;
  var _depth = 0;

  function currentStyle () {
    return _style;
  }

  for (var i = 0; i < tokens.length; i++) {
    var _token = tokens[i];
    if (_token.tag === undefined) {
      writer.text(decodeEntities(_token.text), currentStyle());
      continue;
    }
    var _isBlock = BLOCK_TAGS.indexOf(_token.tag) !== -1;

    if (_token.tag === 'br' && _token.closing === false) {
      writer.lineBreak();
      continue;
    }
    if (_isBlock === false && INLINE_TAGS[_token.tag] === undefined) {
      continue;
    }
    if (_isBlock === true && _token.closing === false && _token.selfClosing === false) {
      _depth++;
      if (_token.tag === 'ul' || _token.tag === 'ol') {
        _listStack.push({ ordered : _token.tag === 'ol', index : 0 });
        continue;
      }
      if (_token.tag === 'li') {
        var _list = _listStack[_listStack.length - 1];
        if (_list !== undefined) {
          _list.index++;
          writer.listItem(_list.ordered, _list.index);
          continue;
        }
      }
      writer.paragraph(HEADING_SIZES[_token.tag] === undefined ? null : { size : HEADING_SIZES[_token.tag] });
      continue;
    }
    if (_isBlock === true && (_token.closing === true || _token.selfClosing === true)) {
      if (_depth > 0) {
        _depth--;
      }
      if (_token.tag === 'ul' || _token.tag === 'ol') {
        _listStack.pop();
      }
      continue;
    }
    if (INLINE_TAGS[_token.tag] !== undefined) {
      var _styleName = INLINE_TAGS[_token.tag];
      if (_token.closing === true || _token.selfClosing === true) {
        _style[_styleName] = Math.max(0, (_style[_styleName] || 1) - 1);
      }
      else {
        _style[_styleName] = (_style[_styleName] || 0) + 1;
      }
    }
  }
}

function docxRunProps (style) {
  var _props = '';
  if (style.bold) {
    _props += '<w:b/><w:bCs/>';
  }
  if (style.italic) {
    _props += '<w:i/><w:iCs/>';
  }
  if (style.underline) {
    _props += '<w:u w:val="single"/>';
  }
  if (style.strike) {
    _props += '<w:strike/>';
  }
  if (style.code) {
    _props += '<w:rFonts w:ascii="Consolas" w:hAnsi="Consolas" w:cs="Consolas"/>';
  }
  if (style.link) {
    _props += '<w:color w:val="0563C1"/><w:u w:val="single"/>';
  }
  return _props;
}

function createDocxWriter () {
  var _parts = [];
  var _openProps = null;      // run properties of the run currently open, null: the template's own run
  var _started = false;       // false until the first text is written
  var _pendingParagraph = null;

  // A paragraph break is only written when something follows, otherwise the
  // document would end with an empty paragraph.
  function flushParagraph () {
    if (_pendingParagraph === null) {
      return;
    }
    if (_started === false && _pendingParagraph === '') {
      // leading <p>: keep the paragraph of the template, and its formatting
      _pendingParagraph = null;
      return;
    }
    // the new paragraph starts with a plain run: a style does not carry over
    _parts.push('</w:t></w:r></w:p><w:p>' + _pendingParagraph + '<w:r><w:t>');
    _openProps = '';
    _pendingParagraph = null;
  }

  return {
    text : function (value, style) {
      if (value === '') {
        return;
      }
      flushParagraph();
      var _props = docxRunProps(style);
      if (_started === true && _props !== _openProps) {
        _parts.push('</w:t></w:r><w:r>' + _props + '<w:t>');
        _openProps = _props;
      }
      else if (_started === false && _props !== '') {
        // the template's run carries its own formatting, only replace it when styled
        _parts.push('</w:t></w:r><w:r>' + _props + '<w:t>');
        _openProps = _props;
      }
      else if (_started === false) {
        // plain text: stay in the run of the template, it needs no extra properties
        _openProps = '';
      }
      _parts.push(escapeXml(value));
      _started = true;
    },

    lineBreak : function () {
      flushParagraph();
      _parts.push('</w:t><w:br/><w:t>');
      _started = true;
    },

    paragraph : function (heading) {
      if (_pendingParagraph !== null) {
        return;
      }
      var _props = heading === null ? '' : '<w:pPr><w:rPr>' + docxRunProps({ bold : true }) + '<w:sz w:val="' + heading.size + '"/><w:szCs w:val="' + heading.size + '"/></w:rPr></w:pPr>';
      _pendingParagraph = _props;
    },

    listItem : function (ordered, index) {
      if (_pendingParagraph !== null) {
        return;
      }
      _pendingParagraph = '<w:pPr><w:ind w:left="360" w:hanging="360"/></w:pPr>';
      this.text(ordered === true ? index + '.  ' : '\u2022  ', {});
    },

    result : function () {
      return _parts.join('');
    }
  };
}

function createOdfWriter () {
  var _parts = [];
  var _pendingParagraph = false;

  function flushParagraph () {
    if (_pendingParagraph === false) {
      return;
    }
    _parts.push('</text:p><text:p>');
    _pendingParagraph = false;
  }

  return {
    // ODF inline styling needs styles declared in styles.xml, so text is kept plain
    text : function (value) {
      if (value === '') {
        return;
      }
      flushParagraph();
      _parts.push(escapeXml(value));
    },

    lineBreak : function () {
      flushParagraph();
      _parts.push('<text:line-break/>');
    },

    paragraph : function () {
      _pendingParagraph = true;
    },

    listItem : function (ordered, index) {
      _pendingParagraph = true;
      this.text(ordered === true ? index + '.  ' : '\u2022  ', {});
    },

    result : function () {
      return _parts.join('');
    }
  };
}

function createTextWriter () {
  var _parts = [];
  var _pendingBreak = false;

  function flushBreak () {
    if (_pendingBreak === true && _parts.length > 0) {
      _parts.push('\n');
    }
    _pendingBreak = false;
  }

  return {
    text : function (value) {
      if (value === '') {
        return;
      }
      flushBreak();
      _parts.push(value);
    },

    lineBreak : function () {
      flushBreak();
      _parts.push('\n');
    },

    paragraph : function () {
      _pendingBreak = true;
    },

    listItem : function (ordered, index) {
      _pendingBreak = true;
      this.text(ordered === true ? index + '.  ' : '\u2022  ', {});
    },

    result : function () {
      return _parts.join('');
    }
  };
}

/**
 * Convert HTML into the markup of the template format.
 *
 * @param  {String} d     the HTML, XML-escaped by the builder
 * @return {String}       document markup, or the HTML itself for HTML templates
 */
function html (d) {
  if (typeof d !== 'string' || d === '') {
    return '';
  }
  var _html = decodeEntities(d.slice(0, MAX_INPUT_LENGTH));

  // an HTML template already is HTML: the editor content is inserted as-is
  if (this.extension === 'html' || this.extension === 'xhtml') {
    return _html;
  }

  var _writer;
  if (this.extension === 'odt' || this.extension === 'fodt') {
    _writer = createOdfWriter();
  }
  else if (this.extension === 'docx') {
    _writer = createDocxWriter();
  }
  else {
    _writer = createTextWriter();
  }
  walk(tokenize(_html), _writer);
  return _writer.result();
}

// the builder escapes the value first, this formatter inserts document markup
html.canInjectXML = true;

module.exports = { html };

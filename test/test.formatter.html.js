var htmlFormatter = require('../formatters/html');
var carbone = require('../lib');
var helper = require('../lib/helper');

describe('formatter', function () {
  describe('html', function () {
    var _docx = { extension : 'docx' };
    var _odt = { extension : 'odt' };

    it('should return an empty string for empty, null or undefined values', function () {
      helper.assert(htmlFormatter.html.call(_docx, ''), '');
      helper.assert(htmlFormatter.html.call(_docx, undefined), '');
      helper.assert(htmlFormatter.html.call(_docx, null), '');
    });

    it('should keep plain text inside the run of the template', function () {
      helper.assert(htmlFormatter.html.call(_docx, '<p>hello</p>'), 'hello');
    });

    it('should escape characters which are not markup', function () {
      // the builder escapes the value before calling the formatter, so the input
      // below is the escaped form of the html 'a & b &lt;c&gt;'
      helper.assert(htmlFormatter.html.call(_docx, 'a &amp;amp; b &amp;lt;c&amp;gt;'), 'a &amp; b &lt;c&gt;');
    });

    it('should decode html entities', function () {
      helper.assert(htmlFormatter.html.call(_docx, 'caf&eacute; &amp; th&eacute;'), 'caf\u00e9 &amp; th\u00e9');
      helper.assert(htmlFormatter.html.call(_docx, '&#233;&#xE9;'), '\u00e9\u00e9');
    });

    it('should turn bold, italic, underline and strikethrough into runs', function () {
      helper.assert(htmlFormatter.html.call(_docx, 'a<b>b</b><i>c</i><u>d</u><s>e</s>'),
        'a</w:t></w:r><w:r><w:b/><w:bCs/><w:t>b</w:t></w:r><w:r><w:i/><w:iCs/><w:t>c</w:t></w:r>'
        + '<w:r><w:u w:val="single"/><w:t>d</w:t></w:r><w:r><w:strike/><w:t>e');
    });

    it('should combine nested inline tags', function () {
      helper.assert(htmlFormatter.html.call(_docx, '<b><i>x</i></b>'),
        '</w:t></w:r><w:r><w:b/><w:bCs/><w:i/><w:iCs/><w:t>x');
    });

    it('should start a new paragraph between blocks, but not before the first one', function () {
      helper.assert(htmlFormatter.html.call(_docx, '<p>a</p><p>b</p>'),
        'a</w:t></w:r></w:p><w:p><w:r><w:t>b');
      // a block break at the end does not add an empty paragraph
      helper.assert(htmlFormatter.html.call(_docx, '<p>a</p>'), 'a');
    });

    it('should convert br into a line break', function () {
      helper.assert(htmlFormatter.html.call(_docx, 'a<br/>b'), 'a</w:t><w:br/><w:t>b');
      helper.assert(htmlFormatter.html.call(_odt, 'a<br/>b'), 'a<text:line-break/>b');
    });

    it('should render bulleted and numbered lists', function () {
      helper.assert(htmlFormatter.html.call(_docx, '<ul><li>one</li><li>two</li></ul>'),
        '</w:t></w:r></w:p><w:p><w:pPr><w:ind w:left="360" w:hanging="360"/></w:pPr><w:r><w:t>\u2022  one'
        + '</w:t></w:r></w:p><w:p><w:pPr><w:ind w:left="360" w:hanging="360"/></w:pPr><w:r><w:t>\u2022  two');
      helper.assert(htmlFormatter.html.call(_docx, '<ol><li>one</li><li>two</li></ol>').indexOf('1.  one') !== -1, true);
      helper.assert(htmlFormatter.html.call(_docx, '<ol><li>one</li><li>two</li></ol>').indexOf('2.  two') !== -1, true);
    });

    it('should render headings as bold paragraphs with a size', function () {
      helper.assert(htmlFormatter.html.call(_docx, '<h2>title</h2>'),
        '</w:t></w:r></w:p><w:p><w:pPr><w:rPr><w:b/><w:bCs/><w:sz w:val="28"/><w:szCs w:val="28"/></w:rPr></w:pPr><w:r><w:t>title');
    });

    it('should drop unknown tags but keep their text', function () {
      helper.assert(htmlFormatter.html.call(_docx, '<article><span>a</span><foo>b</foo></article>'), 'ab');
    });

    it('should drop script, style and comments', function () {
      helper.assert(htmlFormatter.html.call(_docx, 'a<script>var x = 1;</script>b<style>p{}</style>c<!-- hidden -->'), 'abc');
    });

    it('should insert the html as-is in an html template', function () {
      helper.assert(htmlFormatter.html.call({ extension : 'html' }, '<p>a</p>'), '<p>a</p>');
    });

    it('should return text with line breaks for other formats', function () {
      helper.assert(htmlFormatter.html.call({ extension : 'xml' }, '<p>a</p><p>b</p>'), 'a\nb');
    });

    it('should be usable as a marker and inject markup in a docx', function (done) {
      carbone.renderXML('<w:p><w:r><w:t>{d.comment:html}</w:t></w:r></w:p>', { comment : '<b>hi</b>' },
        { extension : 'docx' }, function (err, xml) {
          helper.assert(err + '', 'null');
          helper.assert(xml, '<w:p><w:r><w:t></w:t></w:r><w:r><w:b/><w:bCs/><w:t>hi</w:t></w:r></w:p>');
          done();
        });
    });

    it('should not lose the characters escaped by the builder', function (done) {
      carbone.renderXML('<w:p><w:r><w:t>{d.comment:html}</w:t></w:r></w:p>',
        { comment : '<p>a &amp; b &lt;c&gt; d</p>' }, { extension : 'docx' }, function (err, xml) {
          helper.assert(err + '', 'null');
          helper.assert(xml, '<w:p><w:r><w:t>a &amp; b &lt;c&gt; d</w:t></w:r></w:p>');
          done();
        });
    });
  });
});

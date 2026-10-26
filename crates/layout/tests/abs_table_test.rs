//! Scratch test: an absolute box after a table, as table-anonymous-objects-059.
mod support;

#[test]
fn absolute_green_table_overlays_the_red_one() {
    let found = support::lay_out(
        "
        c position:relative; font-size:2em
          i position:relative; z-index:1; color:red; padding:1px
            s display:table
              a display:table-cell 'Row 1, Col 1'
              b display:table-cell 'Row 1, Col 2'
              g display:table-row-group
                r display:table-row
                  x display:table-cell 'Row 22, Col 1'
          d position:absolute; z-index:2; top:0; color:green; padding:1px
            t display:table; margin:0; padding:0; border:none
              tr display:table-row
                td display:table-cell 'Row 1, Col 1'
        ",
    );
    let d = found["d"];
    let a = found["a"];
    let t = found["t"];
    assert!(
        (d.1 - 0.0).abs() <= 1.0,
        "the absolute box sits at the top: d={d:?} a={a:?} t={t:?}"
    );
}
